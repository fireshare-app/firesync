//! Telling somebody an upload finished, without telling them during a firefight.
//!
//! The suppression is the whole reason this is a module rather than two lines
//! at the call site. Firesync's entire purpose is to run unattended while
//! somebody plays a game, which is precisely when a toast sliding over the
//! screen is least welcome — and a clip finishing is exactly the moment it
//! would happen.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

use crate::config::Settings;

/// Is something currently owning the screen in a way that means "do not
/// interrupt"?
///
/// Windows has a real answer to this and it is the platform Firesync ships for,
/// so that is the one implemented properly. The others return false: guessing
/// wrong in that direction costs somebody a toast at an awkward moment, while
/// guessing wrong the other way silently swallows every notification, which is
/// much harder to notice and much worse.
#[cfg(windows)]
pub fn screen_is_busy() -> bool {
    use windows::Win32::UI::Shell::{
        SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE,
        QUNS_RUNNING_D3D_FULL_SCREEN,
    };
    match unsafe { SHQueryUserNotificationState() } {
        Ok(state) => {
            state == QUNS_RUNNING_D3D_FULL_SCREEN
                || state == QUNS_PRESENTATION_MODE
                || state == QUNS_BUSY
        }
        Err(_) => false,
    }
}

#[cfg(not(windows))]
pub fn screen_is_busy() -> bool {
    false
}

/// One thing worth telling somebody about.
#[derive(Debug, Clone)]
pub struct Note {
    pub title: String,
    pub body: String,
    /// Failures are held back too, but they are counted separately so a summary
    /// can lead with the thing that needs attention.
    pub needs_attention: bool,
}

/// Holds notifications while the screen is busy and delivers what is waiting
/// once it is not.
///
/// A burst is summarised rather than replayed. Coming back from an hour of
/// playing to fourteen separate toasts is worse than no notifications at all,
/// and the interesting number was always "how many" plus "did anything break".
pub struct Notifier {
    app: AppHandle,
    settings: Arc<Mutex<Settings>>,
    pending: Mutex<Vec<Note>>,
    /// Set once a drain loop is running, so there is never more than one.
    draining: AtomicBool,
}

impl Notifier {
    pub fn new(app: AppHandle, settings: Arc<Mutex<Settings>>) -> Arc<Self> {
        Arc::new(Self {
            app,
            settings,
            pending: Mutex::new(Vec::new()),
            draining: AtomicBool::new(false),
        })
    }

    fn wants(&self, note: &Note) -> bool {
        let settings = self.settings.lock().expect("settings mutex");
        if note.needs_attention {
            settings.notifications.on_needs_attention
        } else {
            settings.notifications.on_complete
        }
    }

    fn quiet_while_busy(&self) -> bool {
        self.settings.lock().expect("settings mutex").notifications.quiet_in_fullscreen
    }

    fn group_bursts(&self) -> bool {
        self.settings.lock().expect("settings mutex").notifications.group_bursts
    }

    /// Offer a notification. Whether it is shown now, held, or dropped entirely
    /// is decided here rather than by the caller.
    pub fn post(self: &Arc<Self>, note: Note) {
        if !self.wants(&note) {
            return;
        }

        if self.quiet_while_busy() && screen_is_busy() {
            self.pending.lock().expect("pending mutex").push(note);
            self.start_draining();
            return;
        }

        if self.group_bursts() {
            self.pending.lock().expect("pending mutex").push(note);
            self.start_draining();
            return;
        }

        self.show(&note);
    }

    fn show(&self, note: &Note) {
        let _ = self
            .app
            .notification()
            .builder()
            .title(&note.title)
            .body(&note.body)
            .show();
    }

    /// Wait for the screen to be free, then deliver whatever piled up as one
    /// message.
    fn start_draining(self: &Arc<Self>) {
        if self.draining.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(3)).await;
                if this.quiet_while_busy() && screen_is_busy() {
                    continue;
                }
                // A short settle so a run of uploads finishing together is one
                // message rather than one per straggler.
                if this.group_bursts() {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }

                let held: Vec<Note> = std::mem::take(&mut *this.pending.lock().expect("pending mutex"));
                if held.is_empty() {
                    this.draining.store(false, Ordering::SeqCst);
                    return;
                }
                this.show(&summarise(&held));
            }
        });
    }
}

/// Collapse what was held into one message worth reading.
pub fn summarise(held: &[Note]) -> Note {
    if held.len() == 1 {
        return held[0].clone();
    }

    let failures = held.iter().filter(|n| n.needs_attention).count();
    let done = held.len() - failures;

    let title = match (done, failures) {
        (0, f) => format!("{f} upload{} need attention", plural(f)),
        (d, 0) => format!("{d} upload{} finished", plural(d)),
        (d, f) => format!("{d} uploaded, {f} need{} attention", if f == 1 { "s" } else { "" }),
    };

    // Name a couple so the summary says something, then stop: a toast is not a
    // list view.
    let mut names: Vec<&str> = held.iter().take(2).map(|n| n.body.as_str()).collect();
    if held.len() > names.len() {
        names.push("…");
    }

    Note { title, body: names.join(", "), needs_attention: failures > 0 }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(body: &str, needs_attention: bool) -> Note {
        Note { title: "x".into(), body: body.into(), needs_attention }
    }

    #[test]
    fn one_held_note_is_delivered_as_itself() {
        let only = note("clip.mp4", false);
        let out = summarise(std::slice::from_ref(&only));
        assert_eq!(out.body, "clip.mp4");
        assert!(!out.needs_attention);
    }

    #[test]
    fn a_burst_of_successes_becomes_a_count() {
        let held: Vec<Note> = (0..7).map(|i| note(&format!("c{i}.mp4"), false)).collect();
        let out = summarise(&held);
        assert_eq!(out.title, "7 uploads finished");
        assert!(!out.needs_attention);
        assert!(out.body.ends_with('…'), "{}", out.body);
    }

    /// A summary that buries a failure among successes is the one case where
    /// grouping would do harm, so the count leads with it.
    #[test]
    fn a_mixed_burst_says_what_broke() {
        let mut held: Vec<Note> = (0..3).map(|i| note(&format!("c{i}.mp4"), false)).collect();
        held.push(note("bad.mp4", true));
        let out = summarise(&held);
        assert_eq!(out.title, "3 uploaded, 1 needs attention");
        assert!(out.needs_attention, "a summary containing a failure is itself a failure");
    }

    #[test]
    fn an_all_failure_burst_does_not_claim_any_success() {
        let held: Vec<Note> = (0..2).map(|i| note(&format!("c{i}.mp4"), true)).collect();
        let out = summarise(&held);
        assert_eq!(out.title, "2 uploads need attention");
        assert!(out.needs_attention);
    }
}
