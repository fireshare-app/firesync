use std::time::Duration;

use crate::error::{AppError, Result};

/// How long a discovery call may take. Uploads get their own, much longer,
/// budget — this one only covers the small JSON endpoints.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);

/// Reduce whatever somebody typed into a base URL we can append paths to.
///
/// People paste `fireshare.lan`, `https://fireshare.lan/`, and occasionally
/// `https://fireshare.lan/#/videos` straight out of the browser. All three mean
/// the same instance. A bare host gets https, because a token in a plain HTTP
/// request is readable in transit and defaulting to the insecure scheme is not a
/// favour to anybody.
pub fn normalize_base_url(input: &str) -> Result<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadUrl("Enter the address of your Fireshare instance.".into()));
    }

    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };

    let parsed = url::Url::parse(&with_scheme)
        .map_err(|_| AppError::BadUrl(format!("\"{trimmed}\" is not a valid address.")))?;

    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(AppError::BadUrl(format!(
                "Firesync speaks http and https, not {other}."
            )))
        }
    }

    if parsed.host_str().is_none() {
        return Err(AppError::BadUrl(format!(
            "\"{trimmed}\" does not name a host."
        )));
    }

    // The fragment and query belong to whatever page was open when the address
    // was copied, never to the API.
    let mut base = String::new();
    base.push_str(parsed.scheme());
    base.push_str("://");
    base.push_str(parsed.authority());
    let path = parsed.path().trim_end_matches('/');
    base.push_str(path);

    Ok(base)
}

pub fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(DISCOVERY_TIMEOUT)
        .user_agent(concat!("Firesync/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| AppError::Server(format!("Could not start the HTTP client: {e}")))
}

/// Turn a transport-level failure into something worth reading.
pub fn describe_transport_error(err: &reqwest::Error, base_url: &str) -> AppError {
    let host = url::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| base_url.to_string());

    if err.is_timeout() {
        return AppError::Unreachable(format!(
            "{host} did not answer in time. It may be starting up, or behind a VPN you are not on."
        ));
    }
    if err.is_connect() {
        return AppError::Unreachable(format!(
            "Could not reach {host}. Check the address, and that this machine can see it."
        ));
    }
    if err.is_decode() {
        return AppError::NotFireshare(format!(
            "{host} answered, but not with anything Fireshare's API would send."
        ));
    }
    AppError::Unreachable(format!("Could not talk to {host}: {err}"))
}

/// Map a non-success status onto the meaning the upload-token API gives it.
pub async fn describe_status_error(response: reqwest::Response, host: &str) -> AppError {
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    match status.as_u16() {
        401 => AppError::TokenRejected(
            "That token was rejected. It may have been deleted or regenerated, or the account \
             behind it may have lost its upload permission."
                .into(),
        ),
        429 => {
            let wait = retry_after
                .map(|s| format!(" Try again in {s} seconds."))
                .unwrap_or_default();
            AppError::Throttled(format!(
                "Too many rejected tokens from this machine, so {host} is refusing to check more \
                 for a while.{wait}"
            ))
        }
        // The route simply is not there. Far more likely to be an instance that
        // predates upload tokens than a genuine 404 from the API itself.
        404 => AppError::NotFireshare(format!(
            "{host} has no upload-token API. Firesync needs a Fireshare new enough to have \
             Settings → Security → Upload Tokens."
        )),
        s if (500..600).contains(&s) => AppError::Server(format!(
            "{host} answered {s}. That is a problem on the Fireshare side, not here."
        )),
        s => AppError::Server(format!("{host} answered {s}, which Firesync did not expect.")),
    }
}
