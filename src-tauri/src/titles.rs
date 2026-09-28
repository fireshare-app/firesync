//! Titles for uploads, from a folder's template.
//!
//! Without one, Fireshare titles an upload by its file name, which for most
//! recorders is a timestamp. A template like `{game} — {date}` says what the
//! clip is instead.

use std::path::Path;

/// Fireshare's title column.
const MAX_CHARS: usize = 256;

/// What a template can say about one file.
pub struct Facts<'a> {
    /// The file name without its extension.
    pub file_stem: &'a str,
    pub game: Option<&'a str>,
    /// The watched folder's own name.
    pub folder: &'a str,
    /// When the recording finished, in local time: the file's mtime. Stable
    /// across retries, which matters because every chunk of an upload has to
    /// carry the same title.
    pub finished: chrono::DateTime<chrono::Local>,
}

/// Characters that only ever separate things. A token with nothing to fill it
/// takes the separator beside it with it, so `{game} — {date}` with no game
/// is `2026-09-28` rather than `— 2026-09-28`.
fn is_separator(c: char) -> bool {
    matches!(c, '—' | '–' | '-' | '|' | '·' | ':' | ',' | '/')
}

/// Render `template` for a file. None when the result would be empty, which
/// the caller reads as "let Fireshare use the file name".
pub fn render(template: &str, facts: &Facts<'_>) -> Option<String> {
    let template = template.trim();
    if template.is_empty() {
        return None;
    }

    let mut parts: Vec<String> = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        parts.push(rest[..open].to_string());
        let after = &rest[open..];
        let Some(close) = after.find('}') else {
            parts.push(after.to_string());
            rest = "";
            break;
        };
        let value = match &after[1..close] {
            "filename" => Some(facts.file_stem.to_string()),
            "game" => facts.game.map(str::to_string),
            "folder" => Some(facts.folder.to_string()),
            "date" => Some(facts.finished.format("%Y-%m-%d").to_string()),
            "time" => Some(facts.finished.format("%H:%M").to_string()),
            // Not a token Firesync knows: kept as typed, so a typo shows up in
            // the preview rather than silently vanishing.
            _ => Some(after[..=close].to_string()),
        };
        // An empty token is marked so the tidy-up below can take its separator.
        parts.push(value.filter(|v| !v.trim().is_empty()).unwrap_or_else(|| "\u{0}".into()));
        rest = &after[close + 1..];
    }
    parts.push(rest.to_string());

    let joined = parts.concat();
    let tidy = tidy_separators(&joined);
    let title: String = tidy.chars().take(MAX_CHARS).collect();
    let title = title.trim().to_string();
    (!title.is_empty()).then_some(title)
}

/// Remove each empty-token marker with the run of separators beside it, then
/// any separators left dangling at either end.
fn tidy_separators(text: &str) -> String {
    let words: Vec<&str> = text.split(' ').filter(|w| !w.is_empty()).collect();
    let mut kept: Vec<&str> = Vec::new();
    for (i, word) in words.iter().enumerate() {
        if word.contains('\u{0}') {
            let cleaned = word.replace('\u{0}', "");
            if cleaned.is_empty() || cleaned.chars().all(is_separator) {
                // Take one separator word on the side that has one: after it,
                // unless it is last, in which case the one before it.
                let before_is_sep = kept.last().is_some_and(|w| w.chars().all(is_separator));
                let after_is_sep = words.get(i + 1).is_some_and(|w| w.chars().all(is_separator));
                if !after_is_sep && before_is_sep {
                    kept.pop();
                }
                continue;
            }
            kept.push(word);
            continue;
        }
        let previous_was_marker = i > 0 && {
            let prev = words[i - 1].replace('\u{0}', "");
            words[i - 1].contains('\u{0}') && (prev.is_empty() || prev.chars().all(is_separator))
        };
        if previous_was_marker && word.chars().all(is_separator) {
            continue;
        }
        kept.push(word);
    }
    while kept.first().is_some_and(|w| w.chars().all(is_separator)) {
        kept.remove(0);
    }
    while kept.last().is_some_and(|w| w.chars().all(is_separator)) {
        kept.pop();
    }
    kept.join(" ").replace('\u{0}', "")
}

/// A folder's title for one of its files, or None to leave it to Fireshare.
pub fn for_file(template: Option<&str>, game: Option<&str>, folder: &Path, file: &Path) -> Option<String> {
    let template = template?;
    let finished = std::fs::metadata(file)
        .and_then(|m| m.modified())
        .map(chrono::DateTime::<chrono::Local>::from)
        .unwrap_or_else(|_| chrono::Local::now());
    let file_stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("upload");
    let folder = folder.file_name().and_then(|s| s.to_str()).unwrap_or_default();
    render(template, &Facts { file_stem, game, folder, finished })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn facts(game: Option<&'static str>) -> Facts<'static> {
        Facts {
            file_stem: "ace_on_ascent",
            game,
            folder: "clips",
            finished: chrono::Local.with_ymd_and_hms(2026, 9, 28, 21, 39, 10).unwrap(),
        }
    }

    #[test]
    fn tokens_fill_in() {
        assert_eq!(
            render("{game} — {date} {time}", &facts(Some("VALORANT"))).as_deref(),
            Some("VALORANT — 2026-09-28 21:39")
        );
        assert_eq!(render("{filename} ({folder})", &facts(None)).as_deref(), Some("ace_on_ascent (clips)"));
    }

    #[test]
    fn a_token_with_nothing_to_fill_it_takes_its_separator() {
        assert_eq!(render("{game} — {date}", &facts(None)).as_deref(), Some("2026-09-28"));
        assert_eq!(render("{date} · {game}", &facts(None)).as_deref(), Some("2026-09-28"));
        assert_eq!(
            render("{date} | {game} | {time}", &facts(None)).as_deref(),
            Some("2026-09-28 | 21:39")
        );
    }

    #[test]
    fn nothing_left_means_the_file_name_is_used() {
        assert_eq!(render("{game}", &facts(None)), None);
        assert_eq!(render("   ", &facts(Some("VALORANT"))), None);
    }

    #[test]
    fn an_unknown_token_is_left_as_typed() {
        assert_eq!(render("{gmae} clip", &facts(None)).as_deref(), Some("{gmae} clip"));
    }

    #[test]
    fn a_title_fits_fireshare_s_column() {
        let long = "x".repeat(400);
        assert_eq!(render(&long, &facts(None)).unwrap().chars().count(), 256);
    }

    #[test]
    fn a_title_comes_from_the_file_s_own_time_and_names() {
        let dir = std::env::temp_dir().join(format!("firesync-titles-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("clutch.mp4");
        std::fs::write(&file, b"clip").unwrap();

        let title = for_file(Some("{filename} in {game}"), Some("VALORANT"), &dir, &file);
        assert_eq!(title.as_deref(), Some("clutch in VALORANT"));
        assert_eq!(for_file(None, Some("VALORANT"), &dir, &file), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
