use anyhow::{Context, Result};
use backon::{ExponentialBuilder, Retryable};
use reqwest::{Client, RequestBuilder, Response, StatusCode};

/// Build the shared HTTP client (rustls TLS, sane timeout, identifying UA).
pub fn build_client() -> Result<Client> {
    Client::builder()
        .user_agent("gitea-mirror-sync/0.1")
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("building HTTP client")
}

enum SendError {
    Transport(reqwest::Error),
    Status(StatusCode, String),
}

async fn do_send(rb: RequestBuilder) -> std::result::Result<Response, SendError> {
    let resp = rb.send().await.map_err(SendError::Transport)?;
    let status = resp.status();
    if status.is_success() {
        Ok(resp)
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(SendError::Status(status, body))
    }
}

/// Send a request, retrying transient failures (timeouts, connection errors,
/// HTTP 5xx and 429) with exponential backoff + jitter. The `make` closure must
/// build a fresh `RequestBuilder` on each call so retries are independent.
pub async fn send_retry<F>(make: F) -> Result<Response>
where
    F: Fn() -> RequestBuilder,
{
    let result = (|| async { do_send(make()).await })
        .retry(
            ExponentialBuilder::default()
                .with_max_times(4)
                .with_jitter(),
        )
        .when(|e: &SendError| match e {
            SendError::Transport(err) => err.is_timeout() || err.is_connect(),
            SendError::Status(s, _) => s.is_server_error() || s.as_u16() == 429,
        })
        .await;
    match result {
        Ok(resp) => Ok(resp),
        Err(SendError::Transport(e)) => Err(anyhow::Error::new(e).context("HTTP request failed")),
        Err(SendError::Status(s, body)) => Err(anyhow::anyhow!(
            "API returned HTTP {}: {}",
            s,
            truncate(&body, 600)
        )),
    }
}

/// Fetch a paginated JSON list endpoint. `make_page(page)` builds the request for
/// a given 1-based page. Stops when a page returns fewer than `page_size` items
/// (or `max_pages` is reached). Works across GitHub/GitLab/Gitea/Gitbucket.
pub async fn paginate<F>(
    make_page: F,
    page_size: usize,
    max_pages: usize,
) -> Result<Vec<serde_json::Value>>
where
    F: Fn(usize) -> RequestBuilder,
{
    let mut all = Vec::new();
    for page in 1..=max_pages {
        let resp = send_retry(|| make_page(page)).await?;
        let items: Vec<serde_json::Value> = resp.json().await.context("decoding JSON list page")?;
        let n = items.len();
        all.extend(items);
        if n < page_size {
            break;
        }
    }
    Ok(all)
}

fn truncate(s: &str, max: usize) -> String {
    let t: String = s.chars().take(max).collect();
    if t.len() < s.len() {
        format!("{t}…")
    } else {
        t
    }
}
