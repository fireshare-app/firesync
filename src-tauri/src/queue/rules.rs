use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::{MediaKind, WatchedFolder};

/// Extensions the server would take, as reported by `GET /api/upload/token`.
/// Defaults match Fireshare's own lists so a folder still behaves sensibly
/// before the first successful discovery call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupportedTypes {
    pub video: Vec<String>,
    pub image: Vec<String>,
}

impl Default for SupportedTypes {
    fn default() -> Self {
        Self {
            video: ["mp4", "m4v", "mov", "webm"].iter().map(|s| s.to_string()).collect(),
            image: ["png", "jpg", "jpeg", "webp", "gif"].iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// Suffixes recorders use while a file is still being written. These are not
/// "unsupported types" — they are a file that has not finished existing yet, and
/// the rename that follows is what we actually want to act on.
const IN_PROGRESS_SUFFIXES: &[&str] =
    &[".tmp", ".part", ".partial", ".crdownload", ".download", ".!ut", ".temp"];

fn human_size(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] =
        [("GB", 1 << 30), ("MB", 1 << 20), ("KB", 1 << 10), ("bytes", 1)];
    for (unit, scale) in UNITS {
        if bytes >= scale {
            let value = bytes as f64 / scale as f64;
            return if unit == "bytes" || value >= 10.0 {
                format!("{:.0} {unit}", value)
            } else {
                format!("{:.1} {unit}", value)
            };
        }
    }
    "0 bytes".to_string()
}

/// `None` accepts the file. `Some(reason)` skips it, and the reason is shown to
/// the person rather than logged and forgotten — "why didn't my clip upload" is
/// the question this whole table exists to answer.
pub fn evaluate(
    folder: &WatchedFolder,
    path: &Path,
    size: u64,
    types: &SupportedTypes,
) -> Option<String> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let lower = name.to_ascii_lowercase();

    if name.starts_with('.') {
        return Some("Hidden file".into());
    }

    if let Some(suffix) = IN_PROGRESS_SUFFIXES.iter().find(|s| lower.ends_with(*s)) {
        return Some(format!("Still being written ({suffix})"));
    }

    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) if !e.is_empty() => e.to_ascii_lowercase(),
        _ => return Some("No file extension".into()),
    };

    // An empty media list means the folder was configured before this field
    // existed; video-only is the conservative reading.
    let wants_video = folder.media.is_empty() || folder.media.contains(&MediaKind::Video);
    let wants_image = folder.media.contains(&MediaKind::Image);

    let is_video = types.video.iter().any(|t| t == &ext);
    let is_image = types.image.iter().any(|t| t == &ext);

    if !is_video && !is_image {
        return Some(format!("Fireshare does not accept .{ext}"));
    }
    if is_video && !wants_video {
        return Some("This folder is set to images only".into());
    }
    if is_image && !wants_image {
        return Some("This folder is set to videos only".into());
    }

    if let Some(min) = folder.min_size_bytes {
        if size < min {
            return Some(format!("Under {}", human_size(min)));
        }
    }
    if let Some(max) = folder.max_size_bytes {
        if size > max {
            return Some(format!("Over {}", human_size(max)));
        }
    }

    None
}

/// Whether this path is one the watcher should even wait on. Keeps the settle
/// loop off files that can never be uploaded, so a folder full of `.log` noise
/// does not spawn a waiter per write.
pub fn worth_settling(path: &Path, types: &SupportedTypes) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name.starts_with('.') {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    if IN_PROGRESS_SUFFIXES.iter().any(|s| lower.ends_with(*s)) {
        return false;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            let ext = ext.to_ascii_lowercase();
            types.video.iter().any(|t| t == &ext) || types.image.iter().any(|t| t == &ext)
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn folder() -> WatchedFolder {
        WatchedFolder {
            id: "f1".into(),
            path: PathBuf::from("/tmp/watch"),
            enabled: true,
            include_subfolders: false,
            media: vec![MediaKind::Video],
            dest_folder: None,
            game: None,
            min_size_bytes: Some(5 * 1024 * 1024),
            max_size_bytes: Some(8 * 1024 * 1024 * 1024),
            after_upload: crate::config::AfterUpload::Keep,
            auto_sort_by_game: false,
        }
    }

    #[test]
    fn accepts_a_normal_clip() {
        let t = SupportedTypes::default();
        assert_eq!(evaluate(&folder(), Path::new("/tmp/watch/clip.mp4"), 900 << 20, &t), None);
    }

    #[test]
    fn rejects_below_the_floor_with_a_readable_reason() {
        let t = SupportedTypes::default();
        let r = evaluate(&folder(), Path::new("/tmp/watch/thumb.mp4"), 240 << 10, &t);
        assert_eq!(r.as_deref(), Some("Under 5.0 MB"));
    }

    #[test]
    fn rejects_above_the_ceiling() {
        let t = SupportedTypes::default();
        let r = evaluate(&folder(), Path::new("/tmp/watch/huge.mp4"), 9 * (1 << 30), &t);
        assert_eq!(r.as_deref(), Some("Over 8.0 GB"));
    }

    #[test]
    fn rejects_an_unsupported_container() {
        let t = SupportedTypes::default();
        let r = evaluate(&folder(), Path::new("/tmp/watch/raw.mkv"), 900 << 20, &t);
        assert_eq!(r.as_deref(), Some("Fireshare does not accept .mkv"));
    }

    #[test]
    fn rejects_images_in_a_video_only_folder() {
        let t = SupportedTypes::default();
        let r = evaluate(&folder(), Path::new("/tmp/watch/shot.png"), 900 << 20, &t);
        assert_eq!(r.as_deref(), Some("This folder is set to videos only"));
    }

    #[test]
    fn treats_a_recorder_temp_file_as_unfinished_not_unsupported() {
        let t = SupportedTypes::default();
        let r = evaluate(&folder(), Path::new("/tmp/watch/clip.mp4.tmp"), 900 << 20, &t);
        assert_eq!(r.as_deref(), Some("Still being written (.tmp)"));
        assert!(!worth_settling(Path::new("/tmp/watch/clip.mp4.tmp"), &t));
    }

    #[test]
    fn worth_settling_only_for_media() {
        let t = SupportedTypes::default();
        assert!(worth_settling(Path::new("/tmp/watch/clip.mp4"), &t));
        assert!(!worth_settling(Path::new("/tmp/watch/notes.log"), &t));
        assert!(!worth_settling(Path::new("/tmp/watch/.hidden.mp4"), &t));
    }
}
