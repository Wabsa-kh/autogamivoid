use crate::client::HttpClient;
use crate::models::{
    parse_admin_games, GameEnvelope, GamivoidGamePatch, Taxonomy,
};
use tracing::{debug, info, warn};

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

    /// Create a game (draft or published per the payload). Returns the saved
    /// listing from the guide's `{ "game": { ... } }` envelope (HTTP 201).
    pub async fn create_game(
        &self,
        body: &serde_json::Value,
    ) -> anyhow::Result<crate::models::GamivoidGameSummary> {
        let slug = body.get("slug").and_then(|v| v.as_str()).unwrap_or("?");
        debug!("Creating game: {slug}");
        let envelope: GameEnvelope = self.http.post("/api/admin/games", body).await?;
        info!(
            "Created game: {} (published={})",
            envelope.game.slug, envelope.game.published
        );
        self.http.pause().await;
        Ok(envelope.game)
    }

    /// Read one owner listing (includes drafts). Returns None on 404.
    pub async fn get_game(&self, slug: &str) -> anyhow::Result<Option<serde_json::Value>> {
        let path = format!("/api/admin/games/{slug}");
        match self.http.send_raw(reqwest::Method::GET, &path, None, None).await {
            Ok(raw) => {
                self.http.pause().await;
                Ok(Some(raw.body))
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

    /// Safe update: GET for the current ETag, then PATCH with `If-Match`.
    /// A 412 (stale ETag) is retried once with the fresh ETag. Returns the
    /// normalized saved listing.
    pub async fn patch_game(
        &self,
        slug: &str,
        patch: &GamivoidGamePatch,
    ) -> anyhow::Result<crate::models::GamivoidGameSummary> {
        debug!("Patching game: {slug}");
        let path = format!("/api/admin/games/{slug}");
        let raw = self.http.patch_with_etag(&path, patch).await?;
        let summary: crate::models::GamivoidGameSummary = serde_json::from_value(raw.body)
            .unwrap_or(crate::models::GamivoidGameSummary {
                slug: slug.to_string(),
                published: false,
                etag: raw.etag,
            });
        info!("Patched game: {slug}");
        self.http.pause().await;
        Ok(summary)
    }

    /// List ALL owner listings (drafts included), following page-based
    /// pagination (page/limit/pages per the OpenAPI spec). Used by reconcile.
    pub async fn list_admin_games(&self) -> anyhow::Result<Vec<crate::models::AdminGame>> {
        let mut all = Vec::new();
        let mut page = 1usize;
        loop {
            let path = format!("/api/admin/games?limit=100&page={page}");
            let raw = self
                .http
                .send_raw(reqwest::Method::GET, &path, None, None)
                .await?;
            let batch = parse_admin_games(&raw.body);
            let got = batch.len();
            all.extend(batch);
            // Advance while a full page comes back; an empty or short page
            // means we've reached the end.
            if got < 100 {
                break;
            }
            page += 1;
            self.http.pause().await;
        }
        info!("Listed {} owner games", all.len());
        Ok(all)
    }

    /// Delete a listing (reconcile cleanup of broken drafts).
    pub async fn delete_game(&self, slug: &str) -> anyhow::Result<()> {
        debug!("Deleting game: {slug}");
        let path = format!("/api/admin/games/{slug}");
        self.http
            .send_raw(reqwest::Method::DELETE, &path, None, None)
            .await?;
        warn!("Deleted game: {slug}");
        self.http.pause().await;
        Ok(())
    }
}

fn is_not_found(err: &anyhow::Error) -> bool {
    err.to_string().contains("HTTP 404")
}
