use serde::Deserialize;
use std::time::Duration;
use tracing::{debug, warn};
use url::Url;

/// One completed HTTP exchange: status, response headers, body.
pub struct RawResponse {
    pub status: u16,
    pub etag: Option<String>,
    pub body: serde_json::Value,
    pub body_text: String,
}

pub struct HttpClient {
    client: reqwest::Client,
    base_url: Url,
    bearer_token: String,
    request_pause: Duration,
    max_retries: u32,
}

impl HttpClient {
    pub fn new(base_url: &str, bearer_token: &str, request_pause_ms: u64) -> anyhow::Result<Self> {
        let base_url = Url::parse(base_url)?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            // Some Cloudflare configurations challenge non-browser user agents
            // outright; identifying as a normal client avoids that class of block.
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36")
            .build()?;

        Ok(Self {
            client,
            base_url,
            bearer_token: bearer_token.to_string(),
            request_pause: Duration::from_millis(request_pause_ms),
            max_retries: 4,
        })
    }

    /// GET returning parsed JSON.
    pub async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> anyhow::Result<T> {
        let raw = self.send_raw(reqwest::Method::GET, path, None, None).await?;
        serde_json::from_value(raw.body)
            .map_err(|e| anyhow::anyhow!("unexpected JSON shape from {path}: {e}"))
    }

    /// POST returning parsed JSON (2xx) or a descriptive error.
    pub async fn post<T: for<'de> Deserialize<'de>, B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> anyhow::Result<T> {
        let payload = serde_json::to_vec(body)?;
        let raw = self
            .send_raw(reqwest::Method::POST, path, Some(payload), None)
            .await?;
        serde_json::from_value(raw.body)
            .map_err(|e| anyhow::anyhow!("unexpected JSON shape from {path}: {e}"))
    }

    /// PATCH with `If-Match`. On 412 (stale ETag) re-fetches the listing,
    /// merges nothing (caller sends full intended change) and retries once.
    pub async fn patch_with_etag<B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> anyhow::Result<RawResponse> {
        let payload = serde_json::to_vec(body)?;

        let get_path = path.to_string();
        let first = self
            .send_raw(reqwest::Method::GET, &get_path, None, None)
            .await?;
        let etag = first.etag.clone();

        let send = |etag: Option<String>| {
            let payload = payload.clone();
            async move {
                self.send_raw(reqwest::Method::PATCH, path, Some(payload), etag)
                    .await
            }
        };

        match send(etag.clone()).await {
            Ok(raw) => Ok(raw),
            Err(e) if is_stale_etag(&e) => {
                debug!("412 stale ETag on {path}; re-fetching and retrying once");
                let fresh = self
                    .send_raw(reqwest::Method::GET, &get_path, None, None)
                    .await?;
                send(fresh.etag).await
            }
            Err(e) => Err(e),
        }
    }

    /// Core request: retries 429/5xx with backoff (honoring Retry-After),
    /// detects Cloudflare challenge pages, returns status+headers+body.
    pub async fn send_raw(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Vec<u8>>,
        if_match: Option<String>,
    ) -> anyhow::Result<RawResponse> {
        let url = self.base_url.join(path)?;
        let mut last_error: Option<anyhow::Error> = None;

        for attempt in 1..=self.max_retries {
            if attempt > 1 {
                let delay = backoff_delay(attempt);
                debug!("Backing off for {}ms before retry {}", delay.as_millis(), attempt);
                tokio::time::sleep(delay).await;
            }

            let mut request = self
                .client
                .request(method.clone(), url.clone())
                .header("Authorization", format!("Bearer {}", self.bearer_token))
                .header("Accept", "application/json");
            if body.is_some() {
                request = request.header("Content-Type", "application/json");
            }
            if let Some(etag) = &if_match {
                request = request.header("If-Match", etag.clone());
            }
            if let Some(body) = &body {
                request = request.body(body.clone());
            }

            let response = match request.send().await {
                Ok(r) => r,
                Err(e) => {
                    let message = e.to_string();
                    last_error = Some(anyhow::anyhow!(message.clone()));
                    warn!("Request failed on attempt {attempt}: {message}");
                    continue;
                }
            };

            let status = response.status().as_u16();
            let etag = response
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);

            if status == 429 || status >= 500 {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok());
                let delay = retry_after
                    .map(Duration::from_secs)
                    .unwrap_or_else(|| backoff_delay(attempt));
                warn!(
                    "Retrying after {} (attempt {attempt}) in {:?}",
                    if status == 429 { "rate limit" } else { "server error" },
                    delay
                );
                tokio::time::sleep(delay).await;
                last_error = Some(anyhow::anyhow!(
                    "{}: HTTP {status} from {url}",
                    if status == 429 { "rate limited" } else { "server error" }
                ));
                continue;
            }

            let bytes = response.bytes().await?;
            let body_text = String::from_utf8_lossy(&bytes).to_string();

            if !(200..300).contains(&status) {
                if is_cloudflare_challenge(&body_text) {
                    anyhow::bail!(
                        "Cloudflare bot challenge blocked HTTP {status} {url}. \
                         The runner's IP is not trusted by gamivoid.site's Cloudflare settings. \
                         Fix: in the Cloudflare dashboard for gamivoid.site add a WAF rule \
                         'URI Path starts with /api/' -> Skip; Bot Fight Mode cannot be \
                         skipped on the free plan (turn it off). See README section \
                         'Cloudflare: letting the automation through'."
                    );
                }
                anyhow::bail!(
                    "API request failed: HTTP {status} {url} — {}",
                    body_text.chars().take(300).collect::<String>()
                );
            }

            let parsed = serde_json::from_str(&body_text).unwrap_or(serde_json::Value::Null);
            return Ok(RawResponse {
                status,
                etag,
                body: parsed,
                body_text,
            });
        }

        let err = last_error
            .unwrap_or_else(|| anyhow::anyhow!("request failed after {} attempts", self.max_retries));
        anyhow::bail!("request failed: {err:#}");
    }

    pub async fn pause(&self) {
        if !self.request_pause.is_zero() {
            tokio::time::sleep(self.request_pause).await;
        }
    }
}

fn is_stale_etag(err: &anyhow::Error) -> bool {
    err.to_string().contains("HTTP 412")
}

fn backoff_delay(attempt: u32) -> Duration {
    let base = Duration::from_millis(1000);
    let jitter = Duration::from_millis(rand_millis());
    base + jitter + Duration::from_millis(100 * attempt as u64)
}

/// Detect Cloudflare's interstitial challenge page ("Just a moment...") so we
/// can emit an actionable error instead of raw HTML.
fn is_cloudflare_challenge(body: &str) -> bool {
    let lower = body.to_lowercase();
    lower.contains("just a moment")
        || lower.contains("challenge-platform")
        || (lower.contains("cloudflare") && lower.contains("cf-chl"))
        || lower.contains("attention required! | cloudflare")
}

fn rand_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    ((t >> 7) % 500) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_with_attempts() {
        let early = backoff_delay(1);
        let late = backoff_delay(4);
        assert!(late > early);
    }

    #[test]
    fn detects_cloudflare_challenge_pages() {
        assert!(is_cloudflare_challenge(
            "<html><title>Just a moment...</title></html>"
        ));
        assert!(is_cloudflare_challenge(
            "<script src=\"/cdn-cgi/challenge-platform/h/b/orchestrate\">"
        ));
        assert!(!is_cloudflare_challenge(
            "{\"categories\":[\"Action\"]}"
        ));
    }

    #[test]
    fn stale_etag_detection() {
        assert!(is_stale_etag(&anyhow::anyhow!("API request failed: HTTP 412 http://x")));
        assert!(!is_stale_etag(&anyhow::anyhow!("API request failed: HTTP 400 http://x")));
    }
}
