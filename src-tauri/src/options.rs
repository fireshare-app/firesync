//! What Fireshare will accept an upload into: its folders, its games, and which
//! folder each game's media lives in.
//!
//! The core keeps this, not the window. The window used to fetch it once, when
//! it first saw a connection — and since closing the window only hides it, that
//! one answer lasted the whole session. A game added in Fireshare never reached
//! the Game picker without a reload. Worse, the queue's copy of the folder rules
//! was filled as a side effect of that same fetch, so when it failed at login
//! (the network not up yet, Fireshare mid-restart) every auto-sorted upload for
//! the rest of the session missed its game's folder.
//!
//! So the list is asked for whenever something is about to rely on it — a
//! picker opening, an upload being filed — and on a slow timer besides, and
//! everything reads the same copy.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::api::discovery::{fetch_options, FolderRules, UploadOptions};
use crate::commands::AppState;
use crate::error::{AppError, Result};

/// How stale the list may get while nothing asks for it.
const REFRESH_EVERY: Duration = Duration::from_secs(15 * 60);

/// How often the background loop looks at whether a refresh is due.
const TICK: Duration = Duration::from_secs(30);

/// Backoff after a failed background refresh: from here, doubling, to the cap.
const FIRST_RETRY: Duration = Duration::from_secs(5);
const MAX_RETRY: Duration = Duration::from_secs(5 * 60);

/// How old the folder rules may be when an auto-sorted upload is filed. Short
/// enough that a rule created in Fireshare is in use within minutes, long
/// enough that a burst of clips costs one request rather than one each.
pub const RULES_MAX_AGE: Duration = Duration::from_secs(5 * 60);

/// What a window is shown: the list, when it is from, and whether the latest
/// attempt to refresh it failed.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionsSnapshot {
    pub options: Option<UploadOptions>,
    /// Unix seconds of Fireshare's last answer.
    pub fetched_at: Option<i64>,
    /// Why the latest attempt failed. Cleared by the next one that works, and
    /// kept alongside the old list rather than replacing it.
    pub error: Option<String>,
}

struct Attempt {
    /// When the request left, which is what decides whether a caller that was
    /// waiting may take its answer.
    started: Instant,
    outcome: std::result::Result<(), AppError>,
}

#[derive(Default)]
struct Held {
    options: Option<UploadOptions>,
    fetched_at: Option<Instant>,
    fetched_unix: Option<i64>,
    last_attempt: Option<Attempt>,
    /// Bumped by `clear`, so a request that was in flight for the old server
    /// cannot land its answer on the new one.
    generation: u64,
}

type Listener = Box<dyn Fn(&OptionsSnapshot) + Send + Sync>;

pub struct OptionsCache {
    held: Mutex<Held>,
    /// Held for the length of a request, so only one is ever in flight.
    fetching: tokio::sync::Mutex<()>,
    listener: Mutex<Option<Listener>>,
}

impl Default for OptionsCache {
    fn default() -> Self {
        Self::new()
    }
}

impl OptionsCache {
    pub fn new() -> Self {
        Self {
            held: Mutex::new(Held::default()),
            fetching: tokio::sync::Mutex::new(()),
            listener: Mutex::new(None),
        }
    }

    /// Called with the new state after every refresh, whether it worked or not,
    /// and after `clear`. The app uses it to tell open windows.
    pub fn on_change(&self, listener: impl Fn(&OptionsSnapshot) + Send + Sync + 'static) {
        *self.listener.lock().expect("options listener mutex") = Some(Box::new(listener));
    }

    pub fn snapshot(&self) -> OptionsSnapshot {
        snapshot_of(&self.lock())
    }

    /// How long ago Fireshare last answered, or None if it never has.
    pub fn age(&self) -> Option<Duration> {
        self.lock().fetched_at.map(|at| at.elapsed())
    }

    /// Forget everything. For a new server or token: its library is not the
    /// old one's, and nothing learned about the old one applies.
    pub fn clear(&self) {
        let snapshot = {
            let mut held = self.lock();
            let generation = held.generation + 1;
            *held = Held { generation, ..Held::default() };
            snapshot_of(&held)
        };
        self.notify(&snapshot);
    }

    /// Ask Fireshare for the current list.
    ///
    /// One request at a time. A caller that arrives while one is in flight
    /// waits for it and then asks again, rather than taking its answer: that
    /// request left before the caller asked, so it may predate whatever the
    /// caller wants to see — the game somebody added a moment ago. Callers that
    /// queued up together do share the one request that follows, because it
    /// left after all of them asked. A burst of pickers opening therefore costs
    /// at most two requests, and nobody is handed an answer older than their
    /// question.
    pub async fn refresh(&self, base_url: &str, token: &str) -> Result<UploadOptions> {
        let asked = Instant::now();
        let _turn = self.fetching.lock().await;

        let generation = {
            let held = self.lock();
            if let Some(attempt) = held.last_attempt.as_ref().filter(|a| a.started >= asked) {
                return match &attempt.outcome {
                    Ok(()) => held.options.clone().ok_or_else(forgotten),
                    Err(e) => Err(e.clone()),
                };
            }
            held.generation
        };

        let started = Instant::now();
        let result = fetch_options(base_url, token).await;

        let snapshot = {
            let mut held = self.lock();
            // Cleared while this was in flight, so the answer is about a server
            // that is no longer the one configured.
            if held.generation != generation {
                return result;
            }
            match &result {
                Ok(options) => {
                    // Routine refreshes are the common case and say nothing new;
                    // the first answer, and the first after a failure, do.
                    let news = !matches!(&held.last_attempt, Some(Attempt { outcome: Ok(()), .. }));
                    let level = if news { log::Level::Info } else { log::Level::Debug };
                    log::log!(
                        level,
                        "Fireshare lists {} games, {} video and {} image folder rules",
                        options.games.len(),
                        options.folder_rules.video.len(),
                        options.folder_rules.image.len()
                    );
                    held.options = Some(options.clone());
                    held.fetched_at = Some(Instant::now());
                    held.fetched_unix = Some(unix_now());
                    held.last_attempt = Some(Attempt { started, outcome: Ok(()) });
                }
                Err(e) => held.last_attempt = Some(Attempt { started, outcome: Err(e.clone()) }),
            }
            snapshot_of(&held)
        };

        self.notify(&snapshot);
        result
    }

    /// The folder rules to file an upload by, fresh enough to trust.
    ///
    /// Asks Fireshare when the copy held is older than `max_age`. If it cannot
    /// be asked, an older copy is still used — rules change about as often as
    /// somebody reorganises their library, and a stale answer beats holding up
    /// every upload — but having no copy at all is an error, so the caller can
    /// wait rather than guess.
    pub async fn rules_for_upload(
        &self,
        base_url: &str,
        token: &str,
        max_age: Duration,
    ) -> Result<FolderRules> {
        Ok(self.fresh(base_url, token, max_age).await?.folder_rules)
    }

    /// The whole list, asked for again if the copy held is older than
    /// `max_age`, and the older copy if Fireshare cannot be asked. An error
    /// only when there is no copy at all.
    pub async fn fresh(&self, base_url: &str, token: &str, max_age: Duration) -> Result<UploadOptions> {
        {
            let held = self.lock();
            if let (Some(options), Some(at)) = (&held.options, held.fetched_at) {
                if at.elapsed() < max_age {
                    return Ok(options.clone());
                }
            }
        }

        match self.refresh(base_url, token).await {
            Ok(options) => Ok(options),
            Err(e) => self.lock().options.clone().ok_or(e),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Held> {
        self.held.lock().expect("options mutex")
    }

    fn notify(&self, snapshot: &OptionsSnapshot) {
        if let Some(listener) = self.listener.lock().expect("options listener mutex").as_ref() {
            listener(snapshot);
        }
    }
}

fn snapshot_of(held: &Held) -> OptionsSnapshot {
    OptionsSnapshot {
        options: held.options.clone(),
        fetched_at: held.fetched_unix,
        error: held
            .last_attempt
            .as_ref()
            .and_then(|a| a.outcome.as_ref().err())
            .map(|e| e.to_string()),
    }
}

/// A successful answer that `clear` threw away before a waiting caller got to
/// read it. The server changed underneath the question.
fn forgotten() -> AppError {
    AppError::NotConnected("The Fireshare connection changed while this was being asked.".into())
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Keep the list from going stale while nothing is asking for it, and get a
/// first copy as soon as Fireshare can be reached.
///
/// Retried with backoff rather than tried once, because login is exactly when
/// the network is least likely to be up yet — and a single failed attempt there
/// is what used to leave auto-sort without its rules all session.
///
/// A token Fireshare has refused is the exception. It stays refused until
/// somebody reconnects, which changes the token or the address, so it is not
/// asked about again until then: every retry would only add another "rejected
/// an upload token" warning to Fireshare's log. The queue stops at the same
/// answer, for the same reason.
pub fn spawn_refresh_loop(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut retry_in = FIRST_RETRY;
        let mut refused: Option<(String, String)> = None;
        loop {
            let state = app.state::<AppState>();
            let due = state.options.age().is_none_or(|age| age >= REFRESH_EVERY);

            let wait = match (due, state.credentials()) {
                (true, Ok(creds)) if refused.as_ref() != Some(&creds) => match state
                    .options
                    .refresh(&creds.0, &creds.1)
                    .await
                {
                    Ok(_) => {
                        retry_in = FIRST_RETRY;
                        refused = None;
                        TICK
                    }
                    Err(AppError::TokenRejected(e)) => {
                        log::warn!("Fireshare refused the upload token; not asking again until it changes: {e}");
                        refused = Some(creds);
                        TICK
                    }
                    Err(e) => {
                        log::warn!("Could not refresh Fireshare's folders and games: {e}");
                        let wait = retry_in;
                        retry_in = (retry_in * 2).min(MAX_RETRY);
                        wait
                    }
                },
                // Not due, or not connected yet. Look again shortly: connecting
                // is what makes the first refresh due.
                _ => TICK,
            };

            tokio::time::sleep(wait).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const OPTIONS_BODY: &str = r#"{
        "default_folder": "uploads",
        "folders": {"video": ["uploads", "valorant"], "image": ["uploads"]},
        "games": [{"id": 1, "name": "VALORANT"}],
        "folder_rules": {"video": [{"folder": "valorant", "game_id": 1, "game": "VALORANT"}], "image": []}
    }"#;

    /// A server that answers every request with the options above, after a
    /// pause long enough for other callers to pile up behind the first. Counts
    /// what it was asked, and stops listening once `deadline` passes so the
    /// test never waits on an accept that is not coming.
    fn counting_server(delay: Duration, deadline: Duration) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        std::thread::spawn(move || {
            let until = Instant::now() + deadline;
            while Instant::now() < until {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        seen.fetch_add(1, Ordering::SeqCst);
                        let _ = crate::queue::tests::read_request(&mut stream);
                        std::thread::sleep(delay);
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{OPTIONS_BODY}",
                            OPTIONS_BODY.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        (format!("http://{addr}"), count)
    }

    /// An address nothing is listening on, so every request is refused.
    fn nobody_home() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{addr}")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pickers_opening_together_cost_at_most_two_requests() {
        let (url, count) = counting_server(Duration::from_millis(250), Duration::from_secs(3));
        let cache = Arc::new(OptionsCache::new());

        let calls: Vec<_> = (0..6)
            .map(|_| {
                let cache = cache.clone();
                let url = url.clone();
                tokio::spawn(async move { cache.refresh(&url, "fsk_test").await })
            })
            .collect();
        for call in calls {
            let options = call.await.unwrap().expect("every caller gets the list");
            assert_eq!(options.games[0].name, "VALORANT");
        }

        let asked = count.load(Ordering::SeqCst);
        assert!((1..=2).contains(&asked), "six callers made {asked} requests");
    }

    /// The whole reason not to share an in-flight request: it left before this
    /// caller asked, so it can predate the thing the caller came to see.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_caller_never_gets_an_answer_older_than_its_question() {
        let (url, count) = counting_server(Duration::from_millis(250), Duration::from_secs(3));
        let cache = Arc::new(OptionsCache::new());

        let first = {
            let cache = cache.clone();
            let url = url.clone();
            tokio::spawn(async move { cache.refresh(&url, "fsk_test").await })
        };
        // Ask once the first request is certainly on its way.
        tokio::time::sleep(Duration::from_millis(100)).await;
        cache.refresh(&url, "fsk_test").await.unwrap();
        first.await.unwrap().unwrap();

        assert_eq!(count.load(Ordering::SeqCst), 2, "the late caller should have asked again");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_refresh_keeps_the_list_and_says_why() {
        let (url, _) = counting_server(Duration::ZERO, Duration::from_secs(2));
        let cache = OptionsCache::new();
        cache.refresh(&url, "fsk_test").await.unwrap();

        assert!(cache.refresh(&nobody_home(), "fsk_test").await.is_err());

        let snapshot = cache.snapshot();
        assert!(snapshot.options.is_some(), "the old list should still be there");
        assert!(snapshot.fetched_at.is_some());
        assert!(snapshot.error.is_some(), "and it should say the refresh failed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fresh_rules_are_used_without_asking_again() {
        let (url, count) = counting_server(Duration::ZERO, Duration::from_secs(2));
        let cache = OptionsCache::new();
        cache.refresh(&url, "fsk_test").await.unwrap();

        // Pointed somewhere that would fail, to prove it is not asked.
        let rules = cache
            .rules_for_upload(&nobody_home(), "fsk_test", Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(rules.folder_for("VALORANT", false).as_deref(), Some("valorant"));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stale_rules_beat_none_when_fireshare_cannot_be_asked() {
        let (url, _) = counting_server(Duration::ZERO, Duration::from_secs(2));
        let cache = OptionsCache::new();
        cache.refresh(&url, "fsk_test").await.unwrap();

        let rules = cache
            .rules_for_upload(&nobody_home(), "fsk_test", Duration::ZERO)
            .await
            .expect("the stale copy should be used");
        assert_eq!(rules.folder_for("VALORANT", false).as_deref(), Some("valorant"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_rules_at_all_is_an_error_not_an_empty_list() {
        let cache = OptionsCache::new();
        let result =
            cache.rules_for_upload(&nobody_home(), "fsk_test", RULES_MAX_AGE).await;
        assert!(
            matches!(result, Err(AppError::Unreachable(_))),
            "an empty rule list would silently send every clip to the default folder"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn clearing_forgets_the_old_server_s_list() {
        let (url, _) = counting_server(Duration::ZERO, Duration::from_secs(2));
        let cache = OptionsCache::new();
        cache.refresh(&url, "fsk_test").await.unwrap();

        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        cache.on_change(move |s| log.lock().unwrap().push(s.options.is_some()));
        cache.clear();

        assert!(cache.snapshot().options.is_none());
        assert!(cache.age().is_none());
        assert_eq!(*seen.lock().unwrap(), vec![false], "open windows should hear about it");
    }
}
