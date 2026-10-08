//! The game for a file, in a folder that keeps one subfolder per game.
//!
//! Most recorders file each game's clips in a subfolder named after it —
//! `Clips\VALORANT`, `Clips\ARC Raiders`. Watching each of those as its own
//! folder, with its own copy of the same rules, meant seven cards that said
//! the same thing, and a game played for the first time uploaded nothing until
//! somebody noticed and added an eighth.
//!
//! So a folder can take each file's game from the name of the subfolder it is
//! in. Recorders and libraries do not spell games the same way, so the name is
//! matched against the library where it clearly points at one game, and chosen
//! by hand where it does not. A subfolder that matches nothing and has had
//! nothing chosen holds its files back rather than guessing: a clip tagged with
//! the wrong game is worse than one that waits.

use std::path::Path;

use serde::Serialize;

use crate::api::discovery::UploadOptions;
use crate::config::{SubfolderGame, WatchedFolder};

/// Why an upload is waiting: its subfolder names no game the library has. A
/// prefix, so the rows it marks can be found again once that changes. Holds no
/// `%` or `_`, which the ledger's LIKE would read as wildcards.
pub const UNMATCHED: &str = "No game in your library is named like the folder";

pub fn unmatched_reason(subfolder: &str) -> String {
    format!(
        "{UNMATCHED} \"{subfolder}\". Add it in Fireshare, or choose one in the folder's settings."
    )
}

/// Names shorter than this are never matched by containment: "cs" is inside
/// too many things.
const CONTAINS_MIN: usize = 4;

/// The subfolder directly under `root` that `file` is in, or None for a file
/// in the root itself.
///
/// Tried against the root as stored and as the filesystem resolves it, since
/// the watcher reports resolved paths and an older config may hold the raw one.
pub fn subfolder_of(root: &Path, file: &Path) -> Option<String> {
    let relative = file
        .strip_prefix(root)
        .or_else(|_| file.strip_prefix(crate::watcher::canonical(root)))
        .ok()?;
    let mut parts = relative.components();
    let first = parts.next()?;
    // One component is the file's own name: it is in the root.
    parts.next()?;
    match first {
        std::path::Component::Normal(name) => Some(name.to_string_lossy().to_string()),
        _ => None,
    }
}

/// A name reduced to what two spellings of the same game share: letters and
/// digits, lower-cased. "ARC Raiders", "Arc-Raiders" and "arc raiders" are one.
pub fn normalise(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The library game a subfolder's name points at, when it clearly points at
/// one. In order: a game spelled the same, give or take case and punctuation;
/// a Fireshare folder spelled the same, through its rule to its game; and the
/// one game whose name contains the subfolder's, or the other way round. Two
/// games that could both be meant is no match.
pub fn auto_match(subfolder: &str, options: &UploadOptions) -> Option<String> {
    let wanted = normalise(subfolder);
    if wanted.is_empty() {
        return None;
    }

    if let Some(game) = options.games.iter().find(|g| normalise(&g.name) == wanted) {
        return Some(game.name.clone());
    }

    let rules = options.folder_rules.video.iter().chain(options.folder_rules.image.iter());
    if let Some(game) = rules
        .filter(|r| normalise(&r.folder) == wanted)
        .find_map(|r| r.game.as_deref().filter(|g| !g.trim().is_empty()))
    {
        return Some(game.to_string());
    }

    if wanted.chars().count() < CONTAINS_MIN {
        return None;
    }
    let mut close = options.games.iter().filter(|g| {
        let name = normalise(&g.name);
        name.chars().count() >= CONTAINS_MIN && (name.contains(&wanted) || wanted.contains(&name))
    });
    match (close.next(), close.next()) {
        (Some(game), None) => Some(game.name.clone()),
        _ => None,
    }
}

/// How a subfolder's game was settled on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum How {
    /// Picked in the folder's settings.
    Chosen,
    /// Its name matched a game in the library.
    Matched,
    /// Picked in the folder's settings: sent without a game.
    None,
    /// Nothing chosen, and its name matches nothing. Its files wait.
    Unmatched,
}

/// A subfolder's game, and how it came to be that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub game: Option<String>,
    pub how: How,
}

impl Resolved {
    /// Whether there is an answer at all, including "no game".
    pub fn settled(&self) -> bool {
        self.how != How::Unmatched
    }
}

fn chosen<'a>(folder: &'a WatchedFolder, subfolder: &str) -> Option<&'a SubfolderGame> {
    folder
        .subfolder_games
        .iter()
        .find(|s| s.subfolder == subfolder)
        .or_else(|| folder.subfolder_games.iter().find(|s| s.subfolder.eq_ignore_ascii_case(subfolder)))
}

/// The game for one subfolder: the choice made for it, or the match for its
/// name. With no list to match against — the library not asked yet — every
/// subfolder nobody chose for is unmatched.
pub fn resolve(folder: &WatchedFolder, subfolder: &str, options: Option<&UploadOptions>) -> Resolved {
    if let Some(choice) = chosen(folder, subfolder) {
        return match &choice.game {
            Some(game) => Resolved { game: Some(game.clone()), how: How::Chosen },
            None => Resolved { game: None, how: How::None },
        };
    }
    match options.and_then(|o| auto_match(subfolder, o)) {
        Some(game) => Resolved { game: Some(game), how: How::Matched },
        None => Resolved { game: None, how: How::Unmatched },
    }
}

/// One subfolder, for the folder's card and its settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubfolderStatus {
    pub name: String,
    /// The game its files are sent as, when that is settled.
    pub game: Option<String>,
    pub how: How,
    /// What matching by name gives, whatever was chosen, so the settings can
    /// say what going back to that would mean.
    pub matched: Option<String>,
    /// On disk right now. A choice for a subfolder that has gone is kept and
    /// shown, so it can be seen and dropped.
    pub present: bool,
}

/// Every subfolder of the folder with its game: those on disk, and any chosen
/// one that is no longer there.
pub fn status(folder: &WatchedFolder, options: Option<&UploadOptions>) -> Vec<SubfolderStatus> {
    let mut out: Vec<SubfolderStatus> = on_disk(&folder.path)
        .into_iter()
        .map(|name| {
            let resolved = resolve(folder, &name, options);
            let matched = options.and_then(|o| auto_match(&name, o));
            SubfolderStatus { name, game: resolved.game, how: resolved.how, matched, present: true }
        })
        .collect();
    for choice in &folder.subfolder_games {
        if out.iter().any(|s| s.name.eq_ignore_ascii_case(&choice.subfolder)) {
            continue;
        }
        out.push(SubfolderStatus {
            name: choice.subfolder.clone(),
            game: choice.game.clone(),
            how: if choice.game.is_some() { How::Chosen } else { How::None },
            matched: options.and_then(|o| auto_match(&choice.subfolder, o)),
            present: false,
        });
    }
    out
}

/// The folders directly inside `root`, by name. Hidden ones are left out, as
/// hidden files are everywhere else. Empty when the root cannot be read: a
/// drive that is away has no subfolders to speak of.
pub fn on_disk(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// Choices as the settings dialog sends them, made fit to keep: names
/// trimmed, blanks dropped, and one entry per subfolder, the last one winning,
/// since it is the latest thing somebody did.
pub fn tidy_choices(choices: Vec<SubfolderGame>) -> Vec<SubfolderGame> {
    let mut kept: Vec<SubfolderGame> = Vec::new();
    for choice in choices {
        let subfolder = choice.subfolder.trim().to_string();
        if subfolder.is_empty() {
            continue;
        }
        let game = choice.game.map(|g| g.trim().to_string()).filter(|g| !g.is_empty());
        kept.retain(|k| !k.subfolder.eq_ignore_ascii_case(&subfolder));
        kept.push(SubfolderGame { subfolder, game });
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::discovery::{FolderRule, FolderRules, Game};
    use crate::config::{AfterUpload, MediaKind, WatchMode};
    use std::path::PathBuf;

    fn game(id: i64, name: &str) -> Game {
        Game { id, name: name.into(), steamgriddb_id: None }
    }

    fn library() -> UploadOptions {
        UploadOptions {
            default_folder: Some("uploads".into()),
            folders: Default::default(),
            games: vec![
                game(1, "VALORANT"),
                game(2, "ARC Raiders"),
                game(3, "Counter-Strike 2"),
                game(4, "Battlefield 6"),
                game(5, "Battlefield 2042"),
                game(6, "Halo Infinite"),
                game(7, "Portal 2"),
            ],
            folder_rules: FolderRules {
                video: vec![FolderRule {
                    folder: "wardogs".into(),
                    game_id: Some(8),
                    game: Some("War Dogs: Red Alert".into()),
                }],
                image: vec![],
            },
            tags: None,
        }
    }

    fn folder(choices: Vec<SubfolderGame>) -> WatchedFolder {
        WatchedFolder {
            id: "f".into(),
            path: PathBuf::from("/clips"),
            enabled: true,
            include_subfolders: true,
            media: vec![MediaKind::Video],
            dest_folder: None,
            game: None,
            min_size_bytes: None,
            max_size_bytes: None,
            after_upload: AfterUpload::Keep,
            auto_sort_by_game: true,
            game_from_subfolder: true,
            subfolder_games: choices,
            title_template: None,
            tag_ids: Vec::new(),
            watch_mode: WatchMode::Auto,
        }
    }

    fn choice(subfolder: &str, game: Option<&str>) -> SubfolderGame {
        SubfolderGame { subfolder: subfolder.into(), game: game.map(str::to_string) }
    }

    #[test]
    fn the_subfolder_is_the_first_step_below_the_root() {
        let root = Path::new("/clips");
        assert_eq!(subfolder_of(root, Path::new("/clips/VALORANT/ace.mp4")).as_deref(), Some("VALORANT"));
        assert_eq!(
            subfolder_of(root, Path::new("/clips/ARC Raiders/2026/raid.mp4")).as_deref(),
            Some("ARC Raiders"),
            "deeper folders belong to the game folder above them"
        );
        assert_eq!(subfolder_of(root, Path::new("/clips/loose.mp4")), None, "in the root itself");
        assert_eq!(subfolder_of(root, Path::new("/elsewhere/VALORANT/ace.mp4")), None);
    }

    #[test]
    fn spellings_that_differ_only_in_case_and_punctuation_are_one_name() {
        assert_eq!(normalise("ARC Raiders"), "arcraiders");
        assert_eq!(normalise("Arc-Raiders"), "arcraiders");
        assert_eq!(normalise("Counter-Strike 2"), "counterstrike2");
        assert_eq!(normalise("Battlefield™ 6"), "battlefield6");
    }

    #[test]
    fn a_game_spelled_the_same_matches_however_it_is_cased() {
        let lib = library();
        assert_eq!(auto_match("valorant", &lib).as_deref(), Some("VALORANT"));
        assert_eq!(auto_match("Arc Raiders", &lib).as_deref(), Some("ARC Raiders"));
        assert_eq!(auto_match("CounterStrike2", &lib).as_deref(), Some("Counter-Strike 2"));
    }

    /// Somebody who named their Fireshare folder after the recorder's has
    /// already said which game it is.
    #[test]
    fn a_fireshare_folder_spelled_the_same_leads_to_its_game() {
        assert_eq!(auto_match("WARDOGS", &library()).as_deref(), Some("War Dogs: Red Alert"));
    }

    #[test]
    fn a_name_inside_exactly_one_game_s_name_matches_it() {
        let lib = library();
        assert_eq!(auto_match("Halo", &lib).as_deref(), Some("Halo Infinite"));
        assert_eq!(auto_match("Halo Infinite (Steam)", &lib).as_deref(), Some("Halo Infinite"));
    }

    /// Two games it could mean is no match: a wrong tag is worse than none.
    #[test]
    fn a_name_two_games_could_be_meant_by_matches_neither() {
        assert_eq!(auto_match("Battlefield", &library()), None);
    }

    /// "cs2" is in the name of no game and inside too many strings to trust.
    #[test]
    fn a_short_name_is_never_matched_by_containment() {
        assert_eq!(auto_match("cs2", &library()), None);
        assert_eq!(auto_match("", &library()), None);
    }

    #[test]
    fn a_choice_beats_the_match_and_no_game_is_a_choice_too() {
        let f = folder(vec![choice("valorant", Some("Portal 2")), choice("Desktop", None)]);
        let lib = library();
        assert_eq!(
            resolve(&f, "VALORANT", Some(&lib)),
            Resolved { game: Some("Portal 2".into()), how: How::Chosen },
            "chosen, and found without regard to case"
        );
        assert_eq!(resolve(&f, "Desktop", Some(&lib)), Resolved { game: None, how: How::None });
        assert_eq!(
            resolve(&f, "ARC Raiders", Some(&lib)),
            Resolved { game: Some("ARC Raiders".into()), how: How::Matched }
        );
        assert_eq!(resolve(&f, "Nothing Like It", Some(&lib)), Resolved { game: None, how: How::Unmatched });
        assert!(!resolve(&f, "Nothing Like It", Some(&lib)).settled());
    }

    /// Without the library's list only choices can be answered. Everything
    /// else waits for it rather than being sent without a game.
    #[test]
    fn without_the_library_only_a_choice_is_an_answer() {
        let f = folder(vec![choice("cs2", Some("Counter-Strike 2"))]);
        assert_eq!(resolve(&f, "cs2", None).how, How::Chosen);
        assert_eq!(resolve(&f, "VALORANT", None).how, How::Unmatched);
    }

    #[test]
    fn the_status_lists_what_is_on_disk_and_what_was_chosen_for_a_folder_now_gone() {
        let dir = std::env::temp_dir().join(format!("firesync-games-{}", uuid::Uuid::new_v4()));
        for sub in ["VALORANT", "Mystery Game", ".hidden"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::write(dir.join("loose.mp4"), b"x").unwrap();
        let mut f = folder(vec![choice("Old Game", Some("Portal 2"))]);
        f.path = dir.clone();

        let status = status(&f, Some(&library()));
        let names: Vec<&str> = status.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Mystery Game", "VALORANT", "Old Game"], "disk first, in order; hidden left out");
        assert_eq!(status[0].how, How::Unmatched);
        assert_eq!(status[1].how, How::Matched);
        assert_eq!(status[1].game.as_deref(), Some("VALORANT"));
        assert_eq!(status[2].how, How::Chosen);
        assert!(!status[2].present, "chosen for, but not on disk");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn choices_are_trimmed_and_the_last_one_for_a_subfolder_wins() {
        let tidy = tidy_choices(vec![
            choice("  cs2 ", Some(" Counter-Strike 2 ")),
            choice("", Some("VALORANT")),
            choice("CS2", Some("")),
        ]);
        assert_eq!(tidy, vec![choice("CS2", None)]);
    }
}
