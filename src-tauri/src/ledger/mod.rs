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
