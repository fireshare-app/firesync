//! The text behind "Copy diagnostics": everything a bug report needs, and none
//! of what somebody would not want pasted into a public issue.
//!
//! Built from plain facts rather than from the app, so what it says and what it
//! hides can both be tested without one.

use std::path::Path;

use crate::config::{AfterUpload, MediaKind, Settings, WatchedFolder};
use crate::options::OptionsSnapshot;

/// What the queue was doing when the report was taken.
pub struct QueueFacts {
    pub paused: bool,
    pub pause_reason: Option<String>,
    pub queued: i64,
    pub uploading: i64,
    pub failed: i64,
}

pub struct Facts {
    pub version: String,
    pub os: String,
    pub settings: Settings,
    pub queue: QueueFacts,
    pub options: OptionsSnapshot,
    /// Per folder id: state -> count, from the ledger.
    pub folder_counts: Vec<(String, Vec<(String, i64)>)>,
    pub watcher_problems: Vec<String>,
    pub log_lines: Vec<String>,
    pub now_unix: i64,
}

/// Everything identifying that the report replaces with a placeholder.
///
/// Paths give away a home folder and the name on the account; the server
/// address is somebody's own machine, often at home. Neither helps a maintainer
/// more than the placeholder does, except when the bug is the server or the
/// proxy in front of it — which is what `include_server` is for.
#[derive(Default)]
pub struct Masks {
    /// Longest first, so a home folder is replaced before the name inside it.
    replacements: Vec<(String, &'static str)>,
    /// Never meant to appear anywhere. Replaced unconditionally in case it has.
    secret: Option<String>,
}

impl Masks {
    pub fn new(
        home: Option<&Path>,
        os_user: Option<&str>,
        account: Option<&str>,
        server_url: Option<&str>,
        include_server: bool,
        token: Option<&str>,
    ) -> Self {
        let mut replacements: Vec<(String, &'static str)> = Vec::new();

        if let Some(home) = home.map(|h| h.to_string_lossy().trim_end_matches(['/', '\\']).to_string()) {
            if home.len() > 1 {
                replacements.push((home, "~"));
            }
        }
        if !include_server {
            if let Some(host) = server_url
                .and_then(|u| url::Url::parse(u).ok())
                .and_then(|u| u.host_str().map(str::to_string))
            {
                replacements.push((host, "<server>"));
            }
        }
        // A name this short would match inside ordinary words; the home folder
        // replacement still covers it where it matters most.
        for name in [os_user, account].into_iter().flatten() {
            if name.chars().count() >= 3 {
                replacements.push((name.to_string(), "<user>"));
            }
        }

        replacements.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        Self { replacements, secret: token.filter(|t| !t.is_empty()).map(str::to_string) }
    }

    pub fn apply(&self, text: &str) -> String {
        let mut out = match &self.secret {
            Some(secret) => text.replace(secret.as_str(), "<token>"),
            None => text.to_string(),
        };
        for (needle, with) in &self.replacements {
            out = replace_whole(&out, needle, with);
        }
        mask_unc_hosts(&out)
    }
}

/// Characters that continue a word, a host name or a user name. A match with
/// one of these on either side is part of something longer and is left alone:
/// a user called `sam` must not turn `sample.mp4` into `<user>ple.mp4`.
fn continues_word(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// Replace every standalone occurrence of `needle`, ignoring ASCII case. Paths
/// on Windows and macOS are case-insensitive, and so is a host name.
fn replace_whole(text: &str, needle: &str, with: &str) -> String {
    if needle.is_empty() {
        return text.to_string();
    }
    let lower = text.to_ascii_lowercase();
    let target = needle.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    while let Some(found) = lower[from..].find(&target) {
        let start = from + found;
        let end = start + target.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        // A trailing dot ends a sentence as often as it continues a name.
        let after_ok = match after {
            None => true,
            Some('.') => text[end + 1..].chars().next().is_none_or(|c| !continues_word(c)),
            Some(c) => !continues_word(c),
        };
        if before.is_none_or(|c| !continues_word(c)) && after_ok {
            out.push_str(&text[from..start]);
            out.push_str(with);
        } else {
            out.push_str(&text[from..end]);
        }
        from = end;
    }
    out.push_str(&text[from..]);
    out
}

/// `\\nas\recordings` names a machine on somebody's network. Keep the share,
/// lose the machine.
fn mask_unc_hosts(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(r"\\") {
        out.push_str(&rest[..at + 2]);
        let after = &rest[at + 2..];
        let host_len = after
            .find(|c: char| c == '\\' || c.is_whitespace())
            .unwrap_or(after.len());
        let host = &after[..host_len];
        let is_host = !host.is_empty()
            && host != "?"
            && after[host_len..].starts_with('\\')
            && host.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '.' | '_'));
        if is_host {
            out.push_str("<host>");
            rest = &after[host_len..];
        } else {
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn on_off(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

fn size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else {
        format!("{:.1} MB", b / MB)
    }
}

fn ago(now: i64, then: i64) -> String {
    let seconds = (now - then).max(0);
    match seconds {
        s if s < 90 => "just now".into(),
        s if s < 90 * 60 => format!("{} min ago", s / 60),
        s if s < 36 * 3600 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86_400),
    }
}

fn folder_line(folder: &WatchedFolder) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(if folder.enabled { "watching".into() } else { "paused".into() });
    if folder.include_subfolders {
        parts.push("subfolders".into());
    }
    let media = match (folder.media.contains(&MediaKind::Video), folder.media.contains(&MediaKind::Image)) {
        (true, true) => "video + image",
        (false, true) => "image",
        _ => "video",
    };
    parts.push(media.into());
    match (folder.min_size_bytes, folder.max_size_bytes) {
        (Some(min), Some(max)) => parts.push(format!("{}–{}", size(min), size(max))),
        (Some(min), None) => parts.push(format!("over {}", size(min))),
        (None, Some(max)) => parts.push(format!("under {}", size(max))),
        (None, None) => {}
    }
    parts.push(
        match folder.after_upload {
            AfterUpload::Keep => "keep",
            AfterUpload::Trash => "trash after upload",
            AfterUpload::Delete => "delete after upload",
        }
        .into(),
    );
    if folder.game_from_subfolder {
        parts.push(format!("game from subfolder ({} chosen)", folder.subfolder_games.len()));
    } else if let Some(game) = &folder.game {
        parts.push(format!("game {game}"));
    }
    if folder.auto_sort_by_game {
        parts.push("auto-sort".into());
    }
    if let Some(dest) = &folder.dest_folder {
        parts.push(format!("to {dest}"));
    }
    parts.join(" · ")
}

/// The report, unmasked. Callers always pass it through `Masks::apply`.
pub fn report(facts: &Facts) -> String {
    let mut out: Vec<String> = Vec::new();
    let s = &facts.settings;

    out.push(format!("Firesync {} · {}", facts.version, facts.os));

    let server = s.server_url.clone().unwrap_or_else(|| "not connected".into());
    let account = match &s.last_check {
        Some(check) => format!(
            " · last check: signed in as {}, images {}",
            check.username,
            on_off(check.images_enabled)
        ),
        None => String::new(),
    };
    out.push(format!("Server     {server}{account}"));

    let options = match (&facts.options.options, facts.options.fetched_at) {
        (Some(o), Some(at)) => format!(
            "fetched {} · {} games · {} video and {} image folder rules",
            ago(facts.now_unix, at),
            o.games.len(),
            o.folder_rules.video.len(),
            o.folder_rules.image.len()
        ),
        _ => "not fetched yet".into(),
    };
    let options_error = facts
        .options
        .error
        .as_ref()
        .map(|e| format!(" · last refresh failed: {e}"))
        .unwrap_or_default();
    out.push(format!("Options    {options}{options_error}"));

    let q = &facts.queue;
    let run = if q.paused {
        format!("paused: {}", q.pause_reason.as_deref().unwrap_or("by you"))
    } else {
        "running".into()
    };
    out.push(format!(
        "Queue      {} uploading · {} queued · {} failed · {run}",
        q.uploading, q.queued, q.failed
    ));
    out.push(format!("Transfers  {} at once", s.transfers.max_concurrent));
    let n = &s.notifications;
    out.push(format!(
        "Notify     finish {} · attention {} · quiet in fullscreen {} · group bursts {}",
        on_off(n.on_complete),
        on_off(n.on_needs_attention),
        on_off(n.quiet_in_fullscreen),
        on_off(n.group_bursts)
    ));
    out.push(format!("Updates    install automatically {}", on_off(s.updates.auto_install)));
    out.push(format!(
        "Startup    launch at login {} · start in tray {}",
        on_off(s.startup.launch_at_login),
        on_off(s.startup.start_in_tray)
    ));

    out.push(String::new());
    out.push(format!("Folders ({})", s.folders.len()));
    for (i, folder) in s.folders.iter().enumerate() {
        out.push(format!("  {}  {}", i + 1, folder.path.display()));
        out.push(format!("     {}", folder_line(folder)));
        let counts = facts
            .folder_counts
            .iter()
            .find(|(id, _)| *id == folder.id)
            .map(|(_, c)| {
                c.iter().map(|(state, n)| format!("{n} {state}")).collect::<Vec<_>>().join(" · ")
            })
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "nothing recorded".into());
        out.push(format!("     {counts}"));
    }

    out.push(String::new());
    if facts.watcher_problems.is_empty() {
        out.push("Watcher    no problems".into());
    } else {
        out.push("Watcher problems".into());
        out.extend(facts.watcher_problems.iter().map(|p| format!("  {p}")));
    }

    out.push(String::new());
    out.push(format!("── Last {} log lines ──", facts.log_lines.len()));
    out.extend(facts.log_lines.iter().cloned());

    out.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::discovery::TokenCheck;
    use std::path::PathBuf;

    fn masks(include_server: bool) -> Masks {
        Masks::new(
            Some(Path::new("/Users/shane")),
            Some("shane"),
            Some("shane_fs"),
            Some("https://v.fireshare.net"),
            include_server,
            Some("fsk_supersecret"),
        )
    }

    #[test]
    fn a_home_folder_becomes_a_tilde() {
        let out = masks(false).apply("settled /Users/shane/Recordings/clip.mp4");
        assert_eq!(out, "settled ~/Recordings/clip.mp4");
    }

    #[test]
    fn a_windows_home_is_masked_whatever_its_case() {
        let m = Masks::new(Some(Path::new(r"C:\Users\Shane")), Some("Shane"), None, None, false, None);
        assert_eq!(
            m.apply(r"trashed c:\users\shane\Videos\ace.mp4"),
            r"trashed ~\Videos\ace.mp4"
        );
    }

    #[test]
    fn names_are_masked_only_as_whole_words() {
        let m = Masks::new(None, Some("sam"), None, None, false, None);
        assert_eq!(m.apply("sam uploaded sample.mp4"), "<user> uploaded sample.mp4");
        assert_eq!(m.apply("signed in as sam."), "signed in as <user>.");
    }

    #[test]
    fn a_name_too_short_to_mask_safely_is_left() {
        let m = Masks::new(None, Some("al"), None, None, false, None);
        assert_eq!(m.apply("al's clip, final.mp4"), "al's clip, final.mp4");
    }

    #[test]
    fn the_server_is_masked_unless_asked_for() {
        let line = "Could not reach v.fireshare.net. Check the address.";
        assert_eq!(masks(false).apply(line), "Could not reach <server>. Check the address.");
        assert_eq!(masks(true).apply(line), line);
    }

    #[test]
    fn a_host_is_not_masked_inside_a_longer_one() {
        let m = Masks::new(None, None, None, Some("http://nas:8080"), false, None);
        assert_eq!(m.apply("nas.local is not nas"), "nas.local is not <server>");
    }

    #[test]
    fn a_network_share_keeps_its_share_and_loses_its_machine() {
        let m = Masks::default();
        assert_eq!(m.apply(r"watching \\nas\recordings\clips"), r"watching \\<host>\recordings\clips");
        assert_eq!(m.apply(r"\\?\C:\long\path"), r"\\?\C:\long\path", "not a host");
    }

    #[test]
    fn the_token_is_replaced_even_where_it_should_never_have_been() {
        assert_eq!(masks(false).apply("Bearer fsk_supersecret"), "Bearer <token>");
    }

    fn facts() -> Facts {
        let folder = WatchedFolder {
            id: "f1".into(),
            path: PathBuf::from("/Users/shane/Recordings/VALORANT"),
            enabled: true,
            include_subfolders: true,
            media: vec![MediaKind::Video],
            dest_folder: Some("clips".into()),
            game: Some("VALORANT".into()),
            min_size_bytes: Some(5 << 20),
            max_size_bytes: None,
            after_upload: AfterUpload::Trash,
            auto_sort_by_game: true,
            game_from_subfolder: false,
            subfolder_games: Vec::new(),
            title_template: None,
            tag_ids: Vec::new(),
            watch_mode: crate::config::WatchMode::Auto,
        };
        let settings = Settings {
            server_url: Some("https://v.fireshare.net".into()),
            last_check: Some(TokenCheck {
                ok: true,
                username: "shane_fs".into(),
                default_folder: Some("uploads".into()),
                images_enabled: true,
                supported_video_types: vec![],
                supported_image_types: vec![],
            }),
            folders: vec![folder],
            ..Settings::default()
        };
        Facts {
            version: "1.1.0".into(),
            os: "Windows 11 (26100) 64-bit".into(),
            settings,
            queue: QueueFacts { paused: false, pause_reason: None, queued: 2, uploading: 1, failed: 0 },
            options: OptionsSnapshot::default(),
            folder_counts: vec![("f1".into(), vec![("done".into(), 142), ("skipped".into(), 2)])],
            watcher_problems: vec![],
            log_lines: vec!["[INFO] settled /Users/shane/Recordings/VALORANT/ace.mp4".into()],
            now_unix: 1_790_000_000,
        }
    }

    #[test]
    fn a_report_says_what_a_maintainer_needs_and_hides_the_rest() {
        let text = masks(false).apply(&report(&facts()));

        assert!(text.starts_with("Firesync 1.1.0 · Windows 11 (26100) 64-bit\n"));
        assert!(text.contains("Server     https://<server> · last check: signed in as <user>, images on"));
        assert!(text.contains("Queue      1 uploading · 2 queued · 0 failed · running"));
        assert!(text.contains("  1  ~/Recordings/VALORANT"));
        assert!(text.contains(
            "watching · subfolders · video · over 5.0 MB · trash after upload · game VALORANT · auto-sort · to clips"
        ));
        assert!(text.contains("     142 done · 2 skipped"));
        assert!(text.contains("── Last 1 log lines ──\n[INFO] settled ~/Recordings/VALORANT/ace.mp4"));

        for leaked in ["shane", "fireshare.net", "fsk_"] {
            assert!(!text.to_lowercase().contains(leaked), "{leaked} leaked into:\n{text}");
        }
    }
}
