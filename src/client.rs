use serde::Deserialize;
use std::time::Duration;
use tracing::{debug, warn};
use url::Url;

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
            .build()?;

        Ok(Self {
            client,
            base_url,
            bearer_token: bearer_token.to_string(),
            request_pause: Duration::from_millis(request_pause_ms),
            max_retries: 4,
        })
    }

    pub async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> anyhow::Result<T> {
        self.authorized_request::<T, ()>(self.base_url.join(path)?, reqwest::Method::GET, None)
            .await
    }

    pub async fn post<T: for<'de> Deserialize<'de>, B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> anyhow::Result<T> {
        self.authorized_request::<T, B>(self.base_url.join(path)?, reqwest::Method::POST, Some(body))
            .await
    }

    pub async fn patch<T: for<'de> Deserialize<'de>, B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> anyhow::Result<T> {
        self.authorized_request::<T, B>(self.base_url.join(path)?, reqwest::Method::PATCH, Some(body))
            .await
    }

    async fn authorized_request<T: for<'de> Deserialize<'de>, B: serde::Serialize>(
        &self,
        url: Url,
        method: reqwest::Method,
        body: Option<&B>,
    ) -> anyhow::Result<T> {
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
                .header("Content-Type", "application/json");

            if let Some(body) = body {
                request = request.body(serde_json::to_string(body)?);
            }

            let response = match request.send().await {
                Ok(r) => r,
                Err(e) => {
                    let message = e.to_string();
                    last_error = Some(anyhow::anyhow!(message.clone()));
                    warn!("Request failed on attempt {}: {message}", attempt);
                    continue;
                }
            };

            let status = response.status();

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                let delay = backoff_delay(attempt);
                let reason = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    "rate limited"
                } else {
                    "server error"
                };
                warn!("Retrying after {reason} (attempt {attempt}) in {:?}", delay);
                tokio::time::sleep(delay).await;
                last_error = Some(anyhow::anyhow!("{reason}: HTTP {status} from {url}"));
                continue;
            }

            let bytes = response.bytes().await?;
            if !status.is_success() {
                let text = String::from_utf8_lossy(&bytes);
                anyhow::bail!("API request failed: {status} {url} — {}", text.chars().take(300).collect::<String>());
            }

            let payload: T = serde_json::from_slice(&bytes)?;
            return Ok(payload);
        }

        let err = last_error.unwrap_or_else(|| {
            anyhow::anyhow!("request failed after {} attempts", self.max_retries)
        });
        anyhow::bail!("request failed: {err:#}");
    }

    pub async fn pause(&self) {
        if !self.request_pause.is_zero() {
            tokio::time::sleep(self.request_pause).await;
        }
    }
}

fn backoff_delay(attempt: u32) -> Duration {
    let base = Duration::from_millis(1000);
    let jitter = Duration::from_millis(rand_millis());
    base + jitter + Duration::from_millis(100 * attempt as u64)
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
}
