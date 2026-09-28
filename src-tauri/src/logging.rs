//! Where Firesync writes down what it did, so that a bug report has something
//! in it.
//!
//! Before this, every diagnostic went to stderr. A Windows release build is a
//! GUI-subsystem program, so its stderr goes nowhere at all, and with
//! `panic = "abort"` a crash did not even leave a message behind. Somebody
//! whose clips stopped uploading had nothing to send.
//!
//! Now there is a small rotating log in the OS's log folder, and a panic is
//! written into it before the process goes.

use std::path::{Path, PathBuf};

use log::LevelFilter;
use tauri_plugin_log::{RotationStrategy, Target, TargetKind, TimezoneStrategy};

/// The log file's base name. The current file is `firesync.log`; finished ones
/// get a date added.
pub const FILE_NAME: &str = "firesync";

/// Each file stops here and a new one starts, so no single file is unwieldy to
/// attach to an issue.
const MAX_FILE_BYTES: u128 = 2 * 1024 * 1024;

/// Finished files kept beside the current one. Five files of 2 MB is a ceiling
/// of 10 MB, which is weeks of an ordinary machine's uploads.
const KEEP_FINISHED: usize = 4;

pub fn plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri_plugin_log::Builder::new()
        .clear_targets()
        .target(Target::new(TargetKind::LogDir { file_name: Some(FILE_NAME.into()) }))
        // Somebody running it from a terminal on Linux sees the same lines.
        .target(Target::new(TargetKind::Stdout))
        .level(LevelFilter::Info)
        // Firesync's own lines are the point. The window and HTTP layers are
        // loud at Info and say nothing a Firesync bug report needs, and nothing
        // below Warn from the HTTP layer can end up carrying a request header.
        .level_for("tao", LevelFilter::Warn)
        .level_for("wry", LevelFilter::Warn)
        .level_for("tauri", LevelFilter::Warn)
        .level_for("reqwest", LevelFilter::Warn)
        .level_for("hyper", LevelFilter::Warn)
        .level_for("hyper_util", LevelFilter::Warn)
        .level_for("rustls", LevelFilter::Warn)
        .max_file_size(MAX_FILE_BYTES)
        .rotation_strategy(RotationStrategy::KeepSome(KEEP_FINISHED))
        .timezone_strategy(TimezoneStrategy::UseLocal)
        .build()
}

/// Write a panic into the log before the process aborts.
///
/// The release profile aborts on panic, so there is no unwinding and nothing
/// runs afterwards — this hook is the only chance. The file target flushes
/// after every line, so the message is on disk before the default hook runs
/// and the process ends.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("no message");
        let place = info
            .location()
            .map(|l| format!(" at {}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let thread = std::thread::current();
        let thread = thread.name().unwrap_or("unnamed");
        log::error!("Firesync crashed on thread {thread}: {message}{place}");
        log::logger().flush();
        previous(info);
    }));
}

/// The log files in `dir`, newest first.
fn log_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with(FILE_NAME) && name.ends_with(".log")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.into_iter().map(|(_, path)| path).collect()
}

/// The last `count` lines written, oldest first.
///
/// Reaches back into the previous file when the current one was only just
/// started, so a report taken straight after a rotation is not nearly empty.
pub fn recent_lines(dir: &Path, count: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for file in log_files(dir) {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let mut these: Vec<String> = text.lines().map(str::to_string).collect();
        these.append(&mut lines);
        lines = these;
        if lines.len() >= count {
            break;
        }
    }
    let skip = lines.len().saturating_sub(count);
    lines.split_off(skip)
}

/// A logger that keeps every line in memory, for tests that need to see what
/// was written. Installed once per test process, at every level — stricter than
/// the app, which keeps the HTTP layer at Warn.
#[cfg(test)]
pub(crate) mod capture {
    use std::sync::{Mutex, Once, OnceLock};

    static LINES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    static INSTALL: Once = Once::new();

    struct Capture;

    impl log::Log for Capture {
        fn enabled(&self, _: &log::Metadata) -> bool {
            true
        }
        fn log(&self, record: &log::Record) {
            if let Some(lines) = LINES.get() {
                lines.lock().unwrap().push(format!("{} {}", record.target(), record.args()));
            }
        }
        fn flush(&self) {}
    }

    pub fn lines() -> &'static Mutex<Vec<String>> {
        let lines = LINES.get_or_init(|| Mutex::new(Vec::new()));
        INSTALL.call_once(|| {
            let _ = log::set_boxed_logger(Box::new(Capture));
            log::set_max_level(log::LevelFilter::Trace);
        });
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("firesync-logs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_lines(path: &Path, range: std::ops::Range<usize>) {
        let text: String = range.map(|i| format!("line {i}\n")).collect();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn the_last_lines_come_back_oldest_first() {
        let dir = scratch();
        write_lines(&dir.join("firesync.log"), 0..500);

        let lines = recent_lines(&dir, 200);
        assert_eq!(lines.len(), 200);
        assert_eq!(lines.first().map(String::as_str), Some("line 300"));
        assert_eq!(lines.last().map(String::as_str), Some("line 499"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_started_moments_ago_is_topped_up_from_the_one_before() {
        let dir = scratch();
        write_lines(&dir.join("firesync_2026-09-28_12-00-00.log"), 0..300);
        // Modification times need to differ for the order to mean anything.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_lines(&dir.join("firesync.log"), 300..350);

        let lines = recent_lines(&dir, 200);
        assert_eq!(lines.len(), 200);
        assert_eq!(lines.first().map(String::as_str), Some("line 150"));
        assert_eq!(lines.last().map(String::as_str), Some("line 349"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn other_files_in_the_folder_are_not_read() {
        let dir = scratch();
        write_lines(&dir.join("firesync.log"), 0..3);
        std::fs::write(dir.join("notes.txt"), "not a log\n").unwrap();

        assert_eq!(recent_lines(&dir, 200), vec!["line 0", "line 1", "line 2"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_folder_is_no_lines_rather_than_an_error() {
        assert!(recent_lines(Path::new("/nonexistent/firesync-logs"), 200).is_empty());
    }
}
