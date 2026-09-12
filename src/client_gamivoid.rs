use crate::client::HttpClient;
use crate::models::{GamivoidGamePatch, GamivoidPublishedGame, Taxonomy};
use tracing::{debug, info};

pub struct GamivoidClient {
    http: HttpClient,
}

impl GamivoidClient {
    pub fn new(api_base: &str, publishing_key: &str, request_pause_ms: u64) -> anyhow::Result<Self> {
        Ok(Self {
            http: HttpClient::new(api_base, publishing_key, request_pause_ms)?,
        })
    }

    pub async fn fetch_taxonomy(&self) -> anyhow::Result<Taxonomy> {
        debug!("Fetching taxonomy");
        let payload: Taxonomy = self.http.get("/api/taxonomy").await?;
        Ok(payload)
    }

    /// Create a published game. Fails on slug collisions so caller can dedupe first.
    pub async fn create_game(&self, body: &serde_json::Value) -> anyhow::Result<GamivoidPublishedGame> {
        debug!(
            "Creating game: {}",
            body.get("slug").and_then(|v| v.as_str()).unwrap_or("?")
        );
        let payload: GamivoidPublishedGame = self.http.post("/api/admin/games", body).await?;
        info!("Created game: {}", payload.slug);
        self.http.pause().await;
        Ok(payload)
    }

    /// Update only the fields present in the patch.
    pub async fn patch_game(&self, slug: &str, patch: &GamivoidGamePatch) -> anyhow::Result<()> {
        debug!("Patching game: {}", slug);
        let body = serde_json::to_value(patch)?;
        let _: serde_json::Value = self
            .http
            .patch(&format!("/api/admin/games/{slug}"), &body)
            .await?;
        info!("Patched game: {}", slug);
        self.http.pause().await;
        Ok(())
    }

    /// Read one listing to decide whether it exists. Returns None on 404.
    pub async fn get_game(&self, slug: &str) -> anyhow::Result<Option<serde_json::Value>> {
        let path = format!("/api/admin/games/{slug}");
        let request = self.http.get::<serde_json::Value>(&path);
        match request.await {
            Ok(payload) => {
                self.http.pause().await;
                Ok(Some(payload))
            }
            Err(e) => {
                if is_not_found(&e) {
                    Ok(None)
                } else {
                    Err(e)
                }
            }
        }
    }
}

fn is_not_found(err: &anyhow::Error) -> bool {
    err.to_string().contains("404")
}
