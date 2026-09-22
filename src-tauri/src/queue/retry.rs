use std::time::Duration;

/// How many times a retryable failure is tried before it becomes a visible
/// failure. Eight attempts at the schedule below spans roughly half an hour,
/// which covers a router reboot or a server restart without looking hung.
pub const MAX_ATTEMPTS: i64 = 8;

const BASE: Duration = Duration::from_secs(2);
const CAP: Duration = Duration::from_secs(15 * 60);

/// Exponential backoff with jitter.
///
/// The jitter is not decoration: without it, a queue that fails as a group —
/// which is what happens when the server goes down — retries as a group too,
/// and arrives back in a thundering herd the moment it returns.
pub fn backoff(attempt: i64) -> Duration {
    let exponent = attempt.clamp(0, 20) as u32;
    let raw = BASE.saturating_mul(2u32.saturating_pow(exponent));
    let capped = raw.min(CAP);

    let jitter: f64 = rand::random::<f64>() * 0.4 - 0.2; // ±20%
    let secs = capped.as_secs_f64() * (1.0 + jitter);
    Duration::from_secs_f64(secs.max(1.0))
}

/// Whether another attempt is worth making.
pub fn should_retry(attempts: i64) -> bool {
    attempts + 1 < MAX_ATTEMPTS
}

/// The message shown while a file is waiting to be tried again, so a backoff
/// reads as a plan rather than a stall.
pub fn waiting_reason(reason: &str, attempts: i64, wait: Duration) -> String {
    let next = attempts + 1;
    let secs = wait.as_secs();
    let when = if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    };
    format!("{reason} Retrying in {when} (attempt {next} of {MAX_ATTEMPTS}).")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_then_stops_growing() {
        // Compare medians across many samples so jitter cannot flip the result.
        let median = |attempt: i64| {
            let mut v: Vec<f64> =
                (0..101).map(|_| backoff(attempt).as_secs_f64()).collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[50]
        };

        assert!(median(0) < median(2), "backoff should grow early on");
        assert!(median(2) < median(5), "backoff should keep growing");

        // Capped, jitter included: never more than 20% over the cap.
        for attempt in 0..20 {
            let d = backoff(attempt);
            assert!(
                d <= CAP.mul_f64(1.21),
                "attempt {attempt} waited {d:?}, past the cap"
            );
        }
    }

    #[test]
    fn backoff_is_never_instant() {
        for attempt in 0..20 {
            assert!(backoff(attempt) >= Duration::from_secs(1));
        }
    }

    #[test]
    fn jitter_actually_varies() {
        let samples: Vec<u64> =
            (0..40).map(|_| backoff(6).as_millis() as u64).collect();
        let first = samples[0];
        assert!(
            samples.iter().any(|s| *s != first),
            "every backoff came back identical — jitter is not applied"
        );
    }

    #[test]
    fn gives_up_after_the_last_attempt() {
        assert!(should_retry(0));
        assert!(should_retry(MAX_ATTEMPTS - 2));
        assert!(!should_retry(MAX_ATTEMPTS - 1));
        assert!(!should_retry(MAX_ATTEMPTS));
    }

    #[test]
    fn the_waiting_message_counts_from_one() {
        let msg = waiting_reason("Could not reach the server.", 0, Duration::from_secs(90));
        assert!(msg.contains("attempt 1 of 8"), "{msg}");
        assert!(msg.contains("1m 30s"), "{msg}");
    }
}
