//! Deciding that a file has finished being written.
//!
//! This is the single most load-bearing piece of the watcher. OBS, ShadowPlay
//! and Medal all create the file first and write into it for minutes afterwards,
//! so a naive create-then-upload ships truncated clips. Worse, it ships them
//! *successfully* — the server accepts a valid-looking short file and the person
//! finds out when they try to watch it.
//!
//! Three signals, in order of how much they are worth:
//!
//! 1. On Linux, inotify's `CloseWrite` means exactly "the writer closed this
//!    file". Nothing else here is as good, and the watcher prefers it.
//! 2. On Windows, opening with no sharing flags fails while another process
//!    holds a handle. That is a real answer rather than an inference.
//! 3. Everywhere, size stability over several polls. On its own this is a guess
//!    — a stalled encoder looks identical to a finished one — which is why it is
//!    the fallback rather than the primary signal.

use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct SettleConfig {
    pub poll_interval: Duration,
    /// Consecutive polls at an unchanged size before the file is called done.
    pub stable_polls: u32,
    /// Give up waiting after this. A recorder left running for hours should not
    /// hold a task forever, and the next event on the file starts a fresh wait.
    pub max_wait: Duration,
}

impl Default for SettleConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(2),
            stable_polls: 3,
            max_wait: Duration::from_secs(30 * 60),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// Finished, with the size and mtime it finished at.
    Ready { size: u64, mtime: i64 },
    /// Deleted or renamed away while we waited. A rename produces its own event
    /// at the new path, so there is nothing to chase here.
    Vanished,
    /// Still growing after `max_wait`.
    TimedOut,
}

/// Can this file be opened with no other process holding it?
///
/// Only Windows gives a real answer. Unix has no mandatory locking, so an
/// advisory check would be a coin flip dressed up as a signal — better to say
/// "unknown" and let size stability decide than to invent confidence.
#[cfg(windows)]
fn no_other_writer(path: &Path) -> Option<bool> {
    use std::os::windows::fs::OpenOptionsExt;
    // dwShareMode = 0: fail if anybody else has it open at all.
    Some(std::fs::OpenOptions::new().read(true).share_mode(0).open(path).is_ok())
}

#[cfg(not(windows))]
fn no_other_writer(_path: &Path) -> Option<bool> {
    None
}

fn stat(path: &Path) -> Option<(u64, i64)> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some((meta.len(), mtime))
}

/// Wait until `path` stops changing, or disappears, or we give up.
pub async fn wait_until_settled(path: &Path, cfg: &SettleConfig) -> Settled {
    let deadline = tokio::time::Instant::now() + cfg.max_wait;
    let mut last: Option<(u64, i64)> = None;
    let mut stable = 0u32;

    loop {
        if tokio::time::Instant::now() >= deadline {
            return Settled::TimedOut;
        }
        tokio::time::sleep(cfg.poll_interval).await;

        let Some(current) = stat(path) else {
            return Settled::Vanished;
        };

        if Some(current) == last {
            stable += 1;
        } else {
            stable = 0;
            last = Some(current);
        }

        if stable < cfg.stable_polls {
            continue;
        }

        // Size has held. On Windows, confirm nobody still holds the handle; a
        // stalled encoder with a full buffer looks stable but is not done.
        match no_other_writer(path) {
            Some(false) => {
                stable = 0;
                continue;
            }
            _ => {
                let (size, mtime) = current;
                return Settled::Ready { size, mtime };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fast() -> SettleConfig {
        SettleConfig {
            poll_interval: Duration::from_millis(60),
            stable_polls: 3,
            max_wait: Duration::from_secs(10),
        }
    }

    #[tokio::test]
    async fn waits_for_a_growing_file_and_reports_the_final_size() {
        let dir = std::env::temp_dir().join(format!("firesync-settle-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mp4");

        std::fs::write(&path, vec![0u8; 1024]).unwrap();

        // A recorder still writing while the settle loop runs.
        let writing = {
            let path = path.clone();
            tokio::spawn(async move {
                for _ in 0..8 {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
                    f.write_all(&vec![0u8; 4096]).unwrap();
                }
            })
        };

        let settled = wait_until_settled(&path, &fast()).await;
        writing.await.unwrap();

        match settled {
            Settled::Ready { size, .. } => {
                // It must not have returned at the starting size.
                assert!(size >= 1024 + 8 * 4096, "settled early at {size} bytes");
            }
            other => panic!("expected Ready, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reports_a_file_that_disappears() {
        let dir = std::env::temp_dir().join(format!("firesync-settle-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gone.mp4");
        std::fs::write(&path, b"x").unwrap();

        let p = path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            let _ = std::fs::remove_file(&p);
        });

        assert_eq!(wait_until_settled(&path, &fast()).await, Settled::Vanished);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn gives_up_on_a_file_that_never_stops() {
        let dir = std::env::temp_dir().join(format!("firesync-settle-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("endless.mp4");
        std::fs::write(&path, vec![0u8; 16]).unwrap();

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let path = path.clone();
            let stop = stop.clone();
            tokio::spawn(async move {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&path) {
                        let _ = f.write_all(&vec![0u8; 256]);
                    }
                }
            })
        };

        let cfg = SettleConfig {
            poll_interval: Duration::from_millis(40),
            stable_polls: 3,
            max_wait: Duration::from_millis(500),
        };
        let settled = wait_until_settled(&path, &cfg).await;
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        writer.await.unwrap();

        assert_eq!(settled, Settled::TimedOut);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
