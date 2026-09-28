//! Finding what arrived while nothing was watching.
//!
//! The watcher only hears about files created while it is attached. A clip
//! written while Firesync was quit, crashed, not started yet, or while its drive
//! was unplugged used to get no ledger row at all: not uploaded, and not in the
//! backlog picker either, which lists only rows. It simply did not exist.
//!
//! So whenever a folder is attached — at launch, when its drive comes back,
//! when it is resumed — and every so often besides, the folder is compared
//! against the ledger. Each file the ledger has not seen is sorted by when it
//! appeared: after the folder was last known to be complete means it arrived
//! during a gap, and goes to the queue; before that, or after a pause, it is
//! held for somebody to review.

use std::path::PathBuf;

use crate::config::WatchedFolder;
use crate::error::Result;
use crate::ledger::{FolderMark, Ledger};
use crate::queue::rules::SupportedTypes;

use super::scan_found;
use super::Found;

/// What a pause leaves behind. A paused folder is a choice not to upload, so
/// what lands in it waits to be looked at rather than going when it resumes.
pub const ARRIVED_WHILE_PAUSED: &str = "Arrived while paused";

/// The first scan of a folder added before this version, which has no mark.
/// It cannot tell "arrived while closed" from "arrived while paused", so it
/// holds everything unseen rather than uploading a surprise batch.
pub const FOUND_ON_UPDATE: &str = "Found when Firesync updated";

/// Not in the ledger, but dated before the folder was last known complete:
/// missed long ago, or a type the server has only now started accepting. Not
/// something that just arrived, so not something to send unasked.
pub const DATED_EARLIER: &str = "Found later, dated earlier";

/// Allowance for clocks and for filesystems that store times coarsely (FAT
/// keeps two-second mtimes). Generous is safe: a file seen live already has a
/// row, so widening the window can only send something genuinely unseen.
const SLACK_SECONDS: i64 = 120;

/// How the next scan of a folder should treat what it has not seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Since {
    /// Queue anything that appeared after this moment; hold anything older.
    After(i64),
    /// Hold everything, for this reason.
    Hold(String),
}

impl Since {
    pub fn from_mark(mark: Option<FolderMark>) -> Since {
        match mark {
            None => Since::Hold(FOUND_ON_UPDATE.into()),
            Some(FolderMark { hold_reason: Some(reason), .. }) => Since::Hold(reason),
            Some(FolderMark { watched_through, .. }) => Since::After(watched_through),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Recorded already, and unchanged.
    Skip,
    /// Through the settle gate and the rules, as a live event would be.
    Send,
    /// Into the backlog, with a reason.
    Hold,
}

fn classify(found: &Found, known: Option<&(i64, i64)>, since: &Since) -> Verdict {
    match (known, since) {
        (Some(&(size, mtime)), _) if size == found.size && mtime == found.mtime => Verdict::Skip,
        // Recorded, but the bytes have changed since: a recorder reusing its
        // file name. `observe` already knows what to do with that.
        (Some(_), Since::After(_)) => Verdict::Send,
        // The same, in a folder that was paused. Its changes wait for the
        // watcher, like everything else that happened during the pause.
        (Some(_), Since::Hold(_)) => Verdict::Skip,
        (None, Since::After(complete_at)) if found.appeared() >= complete_at - SLACK_SECONDS => {
            Verdict::Send
        }
        (None, _) => Verdict::Hold,
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CaughtUp {
    /// Sent into the watcher's pipeline, to be settled and ruled on.
    pub sent: usize,
    /// Recorded for review.
    pub held: usize,
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Compare `folder` against the ledger and act on the difference.
///
/// The folder must already be watched: a file landing between the listing and
/// the watch starting would otherwise fall through both. Watching first means
/// such a file is reported twice instead, which the event loop's in-flight set
/// and `observe` already make harmless.
///
/// Afterwards the folder is marked complete as of when the listing began, so
/// the next scan's "after" is exactly what this one could not have seen.
pub fn catch_up(
    folder: &WatchedFolder,
    types: &SupportedTypes,
    ledger: &Ledger,
    send: &tokio::sync::mpsc::UnboundedSender<(String, PathBuf)>,
    since: &Since,
) -> Result<CaughtUp> {
    let listed_at = unix_now();
    let known = ledger.known_files(&folder.id)?;

    let mut to_send = Vec::new();
    let mut to_hold = Vec::new();
    for found in scan_found(folder, types) {
        match classify(&found, known.get(&found.path), since) {
            Verdict::Skip => {}
            Verdict::Send => to_send.push(found.path),
            Verdict::Hold => to_hold.push((found.path, found.size, found.mtime)),
        }
    }

    let reason = match since {
        Since::After(_) => DATED_EARLIER,
        Since::Hold(reason) => reason.as_str(),
    };
    let held = ledger.record_baseline(&folder.id, &to_hold, Some(reason))?;
    let sent = to_send.len();
    for path in to_send {
        let _ = send.send((folder.id.clone(), PathBuf::from(path)));
    }
    ledger.mark_watched(&folder.id, listed_at)?;

    if sent > 0 || held > 0 {
        log::info!(
            "Caught up on {}: {sent} to check and upload, {held} held ({reason})",
            folder.path.display()
        );
    }
    Ok(CaughtUp { sent, held })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AfterUpload, MediaKind};

    fn found(mtime: i64, created: i64) -> Found {
        Found { path: "/w/clip.mp4".into(), size: 10, mtime, created }
    }

    #[test]
    fn a_file_that_appeared_during_the_gap_is_sent() {
        assert_eq!(classify(&found(1_000, 1_000), None, &Since::After(900)), Verdict::Send);
    }

    #[test]
    fn a_file_dated_before_the_mark_is_held() {
        assert_eq!(classify(&found(500, 500), None, &Since::After(900)), Verdict::Hold);
    }

    /// Windows keeps a copied file's mtime but gives it a new creation time. A
    /// clip copied in while Firesync was closed is as new as one copied in
    /// while it was running.
    #[test]
    fn a_copy_counts_from_when_it_was_made_not_from_its_old_date() {
        assert_eq!(classify(&found(100, 1_000), None, &Since::After(900)), Verdict::Send);
    }

    #[test]
    fn a_little_clock_skew_does_not_hold_a_new_file() {
        assert_eq!(classify(&found(850, 850), None, &Since::After(900)), Verdict::Send);
    }

    #[test]
    fn what_the_ledger_already_has_is_left_alone() {
        assert_eq!(classify(&found(500, 500), Some(&(10, 500)), &Since::After(900)), Verdict::Skip);
        assert_eq!(classify(&found(500, 500), Some(&(10, 500)), &Since::Hold("x".into())), Verdict::Skip);
    }

    #[test]
    fn a_reused_name_goes_through_the_pipeline_unless_the_folder_was_paused() {
        assert_eq!(classify(&found(990, 990), Some(&(99, 500)), &Since::After(900)), Verdict::Send);
        assert_eq!(classify(&found(990, 990), Some(&(99, 500)), &Since::Hold("x".into())), Verdict::Skip);
    }

    #[test]
    fn holding_holds_even_the_newest_file() {
        assert_eq!(classify(&found(9_999, 9_999), None, &Since::Hold("x".into())), Verdict::Hold);
    }

    #[test]
    fn a_folder_this_version_has_never_seen_holds_everything() {
        assert_eq!(Since::from_mark(None), Since::Hold(FOUND_ON_UPDATE.into()));
        assert_eq!(
            Since::from_mark(Some(FolderMark { watched_through: 5, hold_reason: None })),
            Since::After(5)
        );
        assert_eq!(
            Since::from_mark(Some(FolderMark { watched_through: 5, hold_reason: Some("p".into()) })),
            Since::Hold("p".into())
        );
    }

    struct Scratch {
        dir: PathBuf,
        folder: WatchedFolder,
        ledger: Ledger,
    }

    fn scratch() -> Scratch {
        let dir = std::env::temp_dir().join(format!("firesync-catchup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = super::super::canonical(&dir);
        let folder = WatchedFolder {
            id: "f1".into(),
            path: dir.clone(),
            enabled: true,
            include_subfolders: false,
            media: vec![MediaKind::Video],
            dest_folder: None,
            game: None,
            min_size_bytes: None,
            max_size_bytes: None,
            after_upload: AfterUpload::Keep,
            auto_sort_by_game: false,
            title_template: None,
            tag_ids: Vec::new(),
            watch_mode: crate::config::WatchMode::Auto,
        };
        let ledger = Ledger::open(&dir.join("l.sqlite")).unwrap();
        Scratch { dir, folder, ledger }
    }

    #[test]
    fn a_clip_written_while_closed_is_sent_and_the_mark_moves_on() {
        let s = scratch();
        s.ledger.mark_watched("f1", unix_now() - 60).unwrap();
        std::fs::write(s.dir.join("while-closed.mp4"), b"clip").unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let result = catch_up(
            &s.folder,
            &SupportedTypes::default(),
            &s.ledger,
            &tx,
            &Since::from_mark(s.ledger.folder_mark("f1").unwrap()),
        )
        .unwrap();

        assert_eq!(result, CaughtUp { sent: 1, held: 0 });
        let (id, path) = rx.try_recv().expect("the clip should be in the pipeline");
        assert_eq!(id, "f1");
        assert!(path.ends_with("while-closed.mp4"));
        let mark = s.ledger.folder_mark("f1").unwrap().unwrap();
        assert!(mark.watched_through >= unix_now() - 5, "the mark should be the scan's start");
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn what_arrived_during_a_pause_is_held_with_its_reason() {
        let s = scratch();
        s.ledger.mark_watched("f1", unix_now() - 600).unwrap();
        s.ledger.hold_next_scan("f1", ARRIVED_WHILE_PAUSED).unwrap();
        std::fs::write(s.dir.join("during-pause.mp4"), b"clip").unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let since = Since::from_mark(s.ledger.folder_mark("f1").unwrap());
        let result = catch_up(&s.folder, &SupportedTypes::default(), &s.ledger, &tx, &since).unwrap();

        assert_eq!(result, CaughtUp { sent: 0, held: 1 });
        assert!(rx.try_recv().is_err(), "nothing should be sent after a pause");
        let rows = s.ledger.baseline_files("f1").unwrap();
        assert_eq!(rows[0].reason.as_deref(), Some(ARRIVED_WHILE_PAUSED));
        assert_eq!(s.ledger.folder_mark("f1").unwrap().unwrap().hold_reason, None);
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn a_second_scan_finds_nothing_left_to_do() {
        let s = scratch();
        std::fs::write(s.dir.join("a.mp4"), b"clip").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        // No mark: the upgrade case, which holds.
        let first = catch_up(&s.folder, &SupportedTypes::default(), &s.ledger, &tx, &Since::from_mark(None))
            .unwrap();
        assert_eq!(first, CaughtUp { sent: 0, held: 1 });

        let since = Since::from_mark(s.ledger.folder_mark("f1").unwrap());
        let second = catch_up(&s.folder, &SupportedTypes::default(), &s.ledger, &tx, &since).unwrap();
        assert_eq!(second, CaughtUp::default());
        let _ = std::fs::remove_dir_all(&s.dir);
    }
}
