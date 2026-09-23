//! Every file Firesync has ever seen, and what it decided about it.
//!
//! This table is what makes two of the product's promises fall out of one
//! mechanism: nothing uploads twice, and files that were already sitting in a
//! folder when you added it stay put until you ask for them. Both are just
//! "have I seen this before, and in what state".

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS files (
  id           INTEGER PRIMARY KEY,
  folder_id    TEXT    NOT NULL,
  path         TEXT    NOT NULL,
  size         INTEGER NOT NULL,
  mtime        INTEGER NOT NULL,
  content_hash TEXT,
  state        TEXT    NOT NULL,
  reason       TEXT,
  attempts     INTEGER NOT NULL DEFAULT 0,
  next_try_at  INTEGER,
  remote_url   TEXT,
  check_sum    TEXT,
  chunks_total INTEGER,
  chunks_done  TEXT,
  observed_at  INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL,
  UNIQUE(folder_id, path)
);
CREATE INDEX IF NOT EXISTS idx_files_state    ON files(state);
CREATE INDEX IF NOT EXISTS idx_files_folder   ON files(folder_id);
CREATE INDEX IF NOT EXISTS idx_files_observed ON files(observed_at DESC);
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileState {
    /// Present when the folder was added. Never uploaded unless asked for.
    Baseline,
    /// Waiting to be sent.
    Queued,
    /// Being sent right now.
    Uploading,
    /// On the server.
    Done,
    /// The server already had it — not a failure.
    Duplicate,
    /// Excluded by this folder's rules. `reason` says which one.
    Skipped,
    /// Tried and did not make it. `reason` says why.
    Failed,
}

impl FileState {
    pub fn as_str(self) -> &'static str {
        match self {
            FileState::Baseline => "baseline",
            FileState::Queued => "queued",
            FileState::Uploading => "uploading",
            FileState::Done => "done",
            FileState::Duplicate => "duplicate",
            FileState::Skipped => "skipped",
            FileState::Failed => "failed",
        }
    }

    fn from_str(s: &str) -> FileState {
        match s {
            "queued" => FileState::Queued,
            "uploading" => FileState::Uploading,
            "done" => FileState::Done,
            "duplicate" => FileState::Duplicate,
            "skipped" => FileState::Skipped,
            "failed" => FileState::Failed,
            _ => FileState::Baseline,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRow {
    pub id: i64,
    pub folder_id: String,
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    pub state: FileState,
    pub reason: Option<String>,
    pub attempts: i64,
    pub observed_at: i64,
    pub updated_at: i64,
}

/// What `observe` did with a file, so the caller (and the debug view) can say
/// why rather than just what.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// First time seen, and it passed the rules.
    Queued,
    /// First time seen, and a rule excluded it.
    Skipped,
    /// Seen before with the same size and mtime. Nothing to do.
    Unchanged,
    /// Seen before, but the bytes changed — a recorder reused the name.
    Requeued,
    /// Known, and deliberately left alone (baseline, or already uploaded).
    Held,
}

pub struct Ledger {
    conn: Mutex<Connection>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn db_err(e: rusqlite::Error) -> AppError {
    AppError::Storage(format!("Ledger: {e}"))
}

impl Ledger {
    pub fn open(path: &Path) -> Result<Ledger> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AppError::Storage(format!("Could not create {}: {e}", parent.display()))
            })?;
        }
        let conn = Connection::open(path).map_err(db_err)?;

        // WAL so a long write cannot block the watcher thread's reads, and
        // NORMAL because losing the last few observations to a power cut is
        // survivable — the folder gets rescanned.
        conn.pragma_update(None, "journal_mode", "WAL").map_err(db_err)?;
        conn.pragma_update(None, "synchronous", "NORMAL").map_err(db_err)?;
        conn.execute_batch(SCHEMA).map_err(db_err)?;

        Ok(Ledger { conn: Mutex::new(conn) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("ledger mutex poisoned")
    }

    /// Record everything already in a folder as `baseline`, in one transaction.
    ///
    /// `INSERT OR IGNORE` so re-running a scan over a folder that has live rows
    /// cannot demote an upload back to baseline.
    pub fn record_baseline(&self, folder_id: &str, entries: &[(String, i64, i64)]) -> Result<usize> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(db_err)?;
        let ts = now();
        let mut inserted = 0;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT OR IGNORE INTO files
                       (folder_id, path, size, mtime, state, observed_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, 'baseline', ?5, ?5)",
                )
                .map_err(db_err)?;
            for (path, size, mtime) in entries {
                inserted += stmt.execute(params![folder_id, path, size, mtime, ts]).map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)?;
        Ok(inserted)
    }

    /// Decide what to do about a file the watcher just settled on.
    ///
    /// `verdict` is the rules' answer: `None` to accept, `Some(reason)` to skip.
    pub fn observe(
        &self,
        folder_id: &str,
        path: &str,
        size: i64,
        mtime: i64,
        verdict: Option<&str>,
    ) -> Result<Outcome> {
        let conn = self.lock();
        let ts = now();

        let existing: Option<(i64, String, i64, i64)> = conn
            .query_row(
                "SELECT id, state, size, mtime FROM files WHERE folder_id = ?1 AND path = ?2",
                params![folder_id, path],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(db_err)?;

        let (state, outcome) = match verdict {
            Some(_) => (FileState::Skipped, Outcome::Skipped),
            None => (FileState::Queued, Outcome::Queued),
        };

        match existing {
            None => {
                conn.execute(
                    "INSERT INTO files
                       (folder_id, path, size, mtime, state, reason, observed_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                    params![folder_id, path, size, mtime, state.as_str(), verdict, ts],
                )
                .map_err(db_err)?;
                Ok(outcome)
            }
            Some((id, prev_state, prev_size, prev_mtime)) => {
                if prev_size == size && prev_mtime == mtime {
                    // Same bytes as last time. Baseline and finished uploads are
                    // both deliberately left alone; anything else is just a
                    // duplicate event from the watcher.
                    return Ok(match FileState::from_str(&prev_state) {
                        FileState::Baseline | FileState::Done | FileState::Duplicate => Outcome::Held,
                        _ => Outcome::Unchanged,
                    });
                }

                // The name was reused with different content. A recorder that
                // overwrites its output would otherwise be silently swallowed by
                // the row from last time.
                conn.execute(
                    "UPDATE files
                        SET size = ?1, mtime = ?2, state = ?3, reason = ?4,
                            attempts = 0, next_try_at = NULL, remote_url = NULL,
                            check_sum = NULL, chunks_total = NULL, chunks_done = NULL,
                            updated_at = ?5
                      WHERE id = ?6",
                    params![size, mtime, state.as_str(), verdict, ts, id],
                )
                .map_err(db_err)?;
                Ok(Outcome::Requeued)
            }
        }
    }

    /// Flip chosen baseline rows into the queue. This is the whole of the
    /// "upload the files that were already here" feature.
    pub fn promote_baseline(&self, folder_id: &str, paths: &[String]) -> Result<usize> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(db_err)?;
        let ts = now();
        let mut promoted = 0;
        {
            let mut stmt = tx
                .prepare(
                    "UPDATE files SET state = 'queued', reason = NULL, updated_at = ?1
                      WHERE folder_id = ?2 AND path = ?3 AND state = 'baseline'",
                )
                .map_err(db_err)?;
            for path in paths {
                promoted += stmt.execute(params![ts, folder_id, path]).map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)?;
        Ok(promoted)
    }

    pub fn recent(&self, limit: i64) -> Result<Vec<FileRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, folder_id, path, size, mtime, state, reason, attempts,
                        observed_at, updated_at
                   FROM files ORDER BY updated_at DESC, id DESC LIMIT ?1",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(FileRow {
                    id: r.get(0)?,
                    folder_id: r.get(1)?,
                    path: r.get(2)?,
                    size: r.get(3)?,
                    mtime: r.get(4)?,
                    state: FileState::from_str(&r.get::<_, String>(5)?),
                    reason: r.get(6)?,
                    attempts: r.get(7)?,
                    observed_at: r.get(8)?,
                    updated_at: r.get(9)?,
                })
            })
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    /// Per-state counts for one folder, for the folder cards.
    pub fn counts(&self, folder_id: &str) -> Result<Vec<(String, i64)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT state, COUNT(*) FROM files WHERE folder_id = ?1 GROUP BY state")
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![folder_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    pub fn forget_folder(&self, folder_id: &str) -> Result<usize> {
        let conn = self.lock();
        conn.execute("DELETE FROM files WHERE folder_id = ?1", params![folder_id])
            .map_err(db_err)
    }
}

/// One queued file, with everything the uploader needs to send it.
#[derive(Debug, Clone)]
pub struct Claim {
    pub id: i64,
    pub folder_id: String,
    pub path: String,
    pub size: i64,
    pub attempts: i64,
}

impl Ledger {
    /// Take up to `limit` files that are due, marking them `uploading` in the
    /// same transaction so two workers cannot claim the same row.
    pub fn claim(&self, limit: i64, exclude_folders: &[String]) -> Result<Vec<Claim>> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(db_err)?;
        let ts = now();

        let claims: Vec<Claim> = {
            let mut stmt = tx
                .prepare(
                    "SELECT id, folder_id, path, size, attempts FROM files
                      WHERE state = 'queued'
                        AND (next_try_at IS NULL OR next_try_at <= ?1)
                      ORDER BY next_try_at IS NULL DESC, next_try_at ASC, id ASC
                      LIMIT ?2",
                )
                .map_err(db_err)?;
            let rows = stmt
                .query_map(params![ts, limit + exclude_folders.len() as i64], |r| {
                    Ok(Claim {
                        id: r.get(0)?,
                        folder_id: r.get(1)?,
                        path: r.get(2)?,
                        size: r.get(3)?,
                        attempts: r.get(4)?,
                    })
                })
                .map_err(db_err)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(db_err)?;
            rows.into_iter()
                .filter(|c| !exclude_folders.contains(&c.folder_id))
                .take(limit as usize)
                .collect()
        };

        for claim in &claims {
            tx.execute(
                "UPDATE files SET state = 'uploading', updated_at = ?1 WHERE id = ?2",
                params![ts, claim.id],
            )
            .map_err(db_err)?;
        }
        tx.commit().map_err(db_err)?;
        Ok(claims)
    }

    pub fn mark_done(&self, id: i64, remote_url: Option<&str>) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE files SET state = 'done', reason = NULL, remote_url = ?1, updated_at = ?2
              WHERE id = ?3",
            params![remote_url, now(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    pub fn mark_duplicate(&self, id: i64, remote_url: Option<&str>) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE files SET state = 'duplicate', reason = 'Already in your library',
                    remote_url = ?1, updated_at = ?2
              WHERE id = ?3",
            params![remote_url, now(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// A failure nothing will fix. It stays visible rather than being retried
    /// forever against a server that has already given its answer.
    pub fn mark_failed(&self, id: i64, reason: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE files SET state = 'failed', reason = ?1, next_try_at = NULL, updated_at = ?2
              WHERE id = ?3",
            params![reason, now(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// Back to the queue, due at `next_try_at`, with the attempt counted.
    pub fn reschedule(&self, id: i64, reason: &str, next_try_at: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE files SET state = 'queued', reason = ?1, attempts = attempts + 1,
                    next_try_at = ?2, updated_at = ?3
              WHERE id = ?4",
            params![reason, next_try_at, now(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// Put a claimed row back untouched — used when the whole queue pauses, so a
    /// file in flight at that moment is not charged an attempt for it.
    pub fn release(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE files SET state = 'queued', updated_at = ?1 WHERE id = ?2 AND state = 'uploading'",
            params![now(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// Anything left `uploading` was interrupted by a crash or a quit, not by a
    /// decision. Put it back so the next run picks it up.
    pub fn requeue_interrupted(&self) -> Result<usize> {
        let conn = self.lock();
        conn.execute(
            "UPDATE files SET state = 'queued', updated_at = ?1 WHERE state = 'uploading'",
            params![now()],
        )
        .map_err(db_err)
    }

    /// Clear the backoff on everything that failed, so "Retry failed" means now.
    pub fn retry_failed(&self) -> Result<usize> {
        let conn = self.lock();
        conn.execute(
            "UPDATE files SET state = 'queued', attempts = 0, next_try_at = NULL, updated_at = ?1
              WHERE state = 'failed'",
            params![now()],
        )
        .map_err(db_err)
    }

    pub fn count_in_state(&self, state: FileState) -> Result<i64> {
        let conn = self.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM files WHERE state = ?1",
            params![state.as_str()],
            |r| r.get(0),
        )
        .map_err(db_err)
    }
}

/// Where a chunked upload got to, so a restart can pick it up rather than
/// sending three gigabytes again.
#[derive(Debug, Clone, Default)]
pub struct ChunkState {
    pub check_sum: Option<String>,
    pub chunks_total: Option<i64>,
    /// Consecutive chunks the server has acknowledged, counting from one.
    pub chunks_done: i64,
}

impl Ledger {
    pub fn chunk_state(&self, id: i64) -> Result<ChunkState> {
        let conn = self.lock();
        conn.query_row(
            "SELECT check_sum, chunks_total, chunks_done FROM files WHERE id = ?1",
            params![id],
            |r| {
                Ok(ChunkState {
                    check_sum: r.get(0)?,
                    chunks_total: r.get(1)?,
                    chunks_done: r
                        .get::<_, Option<String>>(2)?
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0),
                })
            },
        )
        .map_err(db_err)
    }

    pub fn begin_chunks(&self, id: i64, check_sum: &str, total: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE files SET check_sum = ?1, chunks_total = ?2, chunks_done = '0', updated_at = ?3
              WHERE id = ?4",
            params![check_sum, total, now(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// Record that the server has acknowledged everything up to `done`.
    ///
    /// Written after each chunk rather than at the end: this row is the only
    /// record of progress there is, because the server has no route that reports
    /// which parts it is holding.
    pub fn advance_chunks(&self, id: i64, done: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE files SET chunks_done = ?1, updated_at = ?2 WHERE id = ?3",
            params![done.to_string(), now(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// Forget a set entirely. Used when the server has lost its parts and the
    /// file has to start again under a new id.
    pub fn clear_chunks(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE files SET check_sum = NULL, chunks_total = NULL, chunks_done = NULL,
                    updated_at = ?1
              WHERE id = ?2",
            params![now(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }
}

impl Ledger {
    /// Files this folder is deliberately not uploading: the ones that were
    /// already there when it was added.
    pub fn baseline_files(&self, folder_id: &str) -> Result<Vec<FileRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, folder_id, path, size, mtime, state, reason, attempts,
                        observed_at, updated_at
                   FROM files WHERE folder_id = ?1 AND state = 'baseline'
                  ORDER BY mtime DESC",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![folder_id], |r| {
                Ok(FileRow {
                    id: r.get(0)?,
                    folder_id: r.get(1)?,
                    path: r.get(2)?,
                    size: r.get(3)?,
                    mtime: r.get(4)?,
                    state: FileState::from_str(&r.get::<_, String>(5)?),
                    reason: r.get(6)?,
                    attempts: r.get(7)?,
                    observed_at: r.get(8)?,
                    updated_at: r.get(9)?,
                })
            })
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }
}

impl Ledger {
    /// When this folder last had something land on the server, or None if it
    /// never has. Duplicates count: the library gained nothing, but the folder
    /// did do its job.
    pub fn last_upload_at(&self, folder_id: &str) -> Result<Option<i64>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT MAX(updated_at) FROM files
              WHERE folder_id = ?1 AND state IN ('done', 'duplicate')",
            params![folder_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .map_err(db_err)
    }
}

impl Ledger {
    /// Move a folder's rows from one path prefix to another.
    ///
    /// Used when a stored folder path is rewritten — dropping Windows'
    /// extended-length prefix, say. Without this the rows would still be keyed
    /// on the old spelling, every file would look unseen, and a folder's
    /// baseline would be re-queued as new: an unasked-for upload of everything
    /// it was deliberately leaving alone.
    pub fn rewrite_path_prefix(&self, folder_id: &str, old: &str, new: &str) -> Result<usize> {
        let conn = self.lock();
        conn.execute(
            "UPDATE files
                SET path = ?1 || substr(path, ?2), updated_at = ?3
              WHERE folder_id = ?4 AND substr(path, 1, ?5) = ?6",
            params![new, old.len() as i64 + 1, now(), folder_id, old.len() as i64, old],
        )
        .map_err(db_err)
    }
}

#[cfg(test)]
mod path_migration_tests {
    use super::*;

    fn ledger() -> (Ledger, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("firesync-mig-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        (Ledger::open(&dir.join("l.sqlite")).unwrap(), dir)
    }

    /// The whole point: after the move the rows are still found under the new
    /// spelling, so nothing looks unseen and no baseline is re-queued.
    #[test]
    fn rows_follow_their_folder_to_a_new_prefix() {
        let (l, dir) = ledger();
        let old = r"\\?\E:\Segra Game Recordings\Clips\WARDOGS";
        let new = r"E:\Segra Game Recordings\Clips\WARDOGS";

        l.record_baseline(
            "f1",
            &[
                (format!(r"{old}\a.mp4"), 10, 1),
                (format!(r"{old}\b.mp4"), 20, 2),
            ],
        )
        .unwrap();

        assert_eq!(l.rewrite_path_prefix("f1", old, new).unwrap(), 2);

        let paths: Vec<String> = l.recent(10).unwrap().into_iter().map(|r| r.path).collect();
        assert!(paths.contains(&format!(r"{new}\a.mp4")), "{paths:?}");
        assert!(paths.contains(&format!(r"{new}\b.mp4")), "{paths:?}");
        assert!(!paths.iter().any(|p| p.starts_with(r"\\?\")), "a row kept the old prefix");

        // Still baseline: a rename is not a reason to upload anything.
        assert!(l.recent(10).unwrap().iter().all(|r| r.state == FileState::Baseline));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Another folder's rows are not swept along by a shared prefix.
    #[test]
    fn only_the_named_folder_moves() {
        let (l, dir) = ledger();
        let old = r"\\?\E:\Clips";
        l.record_baseline("f1", &[(format!(r"{old}\one.mp4"), 1, 1)]).unwrap();
        l.record_baseline("f2", &[(format!(r"{old}\two.mp4"), 1, 1)]).unwrap();

        assert_eq!(l.rewrite_path_prefix("f1", old, r"E:\Clips").unwrap(), 1);

        let rows = l.recent(10).unwrap();
        let f2 = rows.iter().find(|r| r.folder_id == "f2").unwrap();
        assert!(f2.path.starts_with(r"\\?\"), "f2 should be untouched: {}", f2.path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A path that merely contains the prefix elsewhere is not a match.
    #[test]
    fn only_a_real_prefix_counts() {
        let (l, dir) = ledger();
        l.record_baseline("f1", &[(r"D:\other\file.mp4".to_string(), 1, 1)]).unwrap();
        assert_eq!(l.rewrite_path_prefix("f1", r"\\?\E:\Clips", r"E:\Clips").unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
