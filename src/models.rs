use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceGame {
    pub title: String,
    pub download_url: String,
    pub version: Option<String>,
    pub notes: Option<String>,
    pub source_label: SourceLabel,
}

impl From<SourceGame> for DownloadCandidate {
    fn from(game: SourceGame) -> Self {
        Self {
            url: game.download_url,
            source_label: game.source_label,
            version: game.version,
            notes: game.notes,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SourceLabel {
    Steamrip,
    Steamunlocked,
}

impl std::fmt::Display for SourceLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceLabel::Steamrip => write!(f, "steamrip"),
            SourceLabel::Steamunlocked => write!(f, "steamunlocked"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatchedGame {
    pub normalized_title: String,
    pub raw_title: String,
    pub candidates: Vec<SourceGame>,
    pub best_download: DownloadCandidate,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadCandidate {
    pub url: String,
    pub source_label: SourceLabel,
    pub version: Option<String>,
    pub notes: Option<String>,
}

/// One entry of the API guide's `downloadLinks` array.
#[derive(Clone, Debug, Serialize)]
pub struct DownloadLink {
    pub label: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A complete, guide-shaped listing: every field the publishing API accepts
/// that we can produce. `description` is plain text; `article_md` is safe
/// Markdown (the API renders it and rejects raw HTML).
#[derive(Clone, Debug)]
pub struct EnrichedGame {
    pub raw_title: String,
    pub description: String,
    pub article_md: String,
    pub developer: Option<String>,
    pub publisher: Option<String>,
    pub release_date: Option<String>,
    pub platforms: Vec<String>,
    /// Primary category guess; the workflow maps it to an exact taxonomy value.
    pub category_guess: Option<String>,
    /// Extra taxonomy categories (workflow maps each through exact_match).
    pub category_guesses: Vec<String>,
    pub tags: Vec<String>,
    pub cover_image_url: Option<String>,
    pub cover_alt: Option<String>,
    pub hero_image_url: Option<String>,
    pub featured_image_url: Option<String>,
    pub featured_image_alt: Option<String>,
    pub screenshot_urls: Vec<String>,
    pub screenshot_alts: Vec<String>,
    /// Publish gate: needs 10+ chars when publishing.
    pub minimum: Option<String>,
    pub recommended: Option<String>,
    pub version: Option<String>,
    pub file_size: Option<String>,
    pub storage: Option<String>,
    pub instructions: Vec<String>,
    pub download_links: Vec<DownloadLink>,
    pub source_url: Option<String>,
    pub license_type: String,
    /// SEO title, max 120 chars (guide field: seoTitle).
    pub seo_title: String,
    /// SEO description, max 320 chars (guide field: seoDescription).
    pub seo_description: String,
    pub steam_app_id: Option<u64>,
    pub published_at: Option<String>,
}

/// PATCH body: `Option` fields map directly to "fields omitted are preserved".
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GamivoidGamePatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_links: Option<Vec<DownloadLink>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub article: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seo_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seo_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// Create response envelope per the guide: `{ "game": { ... } }` (HTTP 201).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GameEnvelope {
    pub game: GamivoidGameSummary,
}

/// The part of a saved game we need back from create/patch responses.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GamivoidGameSummary {
    pub slug: String,
    #[serde(default)]
    pub published: bool,
    #[serde(default)]
    pub etag: Option<String>,
}

/// Owner listing shape used by the reconcile action (GET /api/admin/games).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminGame {
    pub slug: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub published: bool,
}

/// Lenient admin-list parsing: accepts `{"games": [...]}` or a bare array.
pub fn parse_admin_games(value: &serde_json::Value) -> Vec<AdminGame> {
    let arr = match value {
        serde_json::Value::Array(arr) => Some(arr.clone()),
        serde_json::Value::Object(map) => map
            .get("games")
            .or_else(|| map.get("data"))
            .and_then(|v| v.as_array())
            .cloned(),
        _ => None,
    };
    arr.unwrap_or_default()
        .into_iter()
        .filter_map(|item| serde_json::from_value::<AdminGame>(item).ok())
        .collect()
}

/// Cursor fields some list responses include.
pub fn parse_cursor(value: &serde_json::Value) -> Option<(bool, Option<String>)> {
    let obj = value.as_object()?;
    let has_more = obj.get("hasMore").and_then(|v| v.as_bool())?;
    let next = obj
        .get("nextCursor")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Some((has_more, next))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexEntry {
    pub slug: String,
    #[serde(default)]
    pub title: Option<String>,
    pub last_version: Option<String>,
    pub last_download_source: Option<SourceLabel>,
    #[serde(default)]
    pub published_at: Option<String>,
    /// True only once the listing is confirmed to exist on the site (a
    /// successful create/patch response or a reconcile hit). Entries written
    /// by older dry-runs carry false and are republished.
    #[serde(default)]
    pub verified: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateFile {
    #[serde(default)]
    pub last_run_at: Option<String>,
    #[serde(default)]
    pub index: HashMap<String, IndexEntry>,
    #[serde(default)]
    pub checkpoint: Vec<String>,
}

/// One row of the exported published-games manifest.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    pub slug: String,
    pub title: Option<String>,
    pub version: Option<String>,
    pub source: Option<String>,
    pub published_at: Option<String>,
}

/// Lenient taxonomy: accepts `{"categories": ["Action", ...]}`,
/// `{"categories": [{"name": "Action"}, ...]}` or a bare array of either.
#[derive(Clone, Debug, Serialize)]
pub struct Taxonomy {
    pub categories: Vec<String>,
}

impl<'de> Deserialize<'de> for Taxonomy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let items: Vec<serde_json::Value> = match &value {
            serde_json::Value::Array(arr) => arr.clone(),
            serde_json::Value::Object(map) => map
                .get("categories")
                .or_else(|| map.get("data"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default(),
            _ => Vec::new(),
        };

        let mut categories = Vec::new();
        for item in items {
            let name = match item {
                serde_json::Value::String(s) => s,
                serde_json::Value::Object(map) => ["name", "slug", "title", "id"]
                    .iter()
                    .find_map(|key| {
                        map.get(*key)
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or_default(),
                _ => continue,
            };
            if !name.trim().is_empty() {
                categories.push(name);
            }
        }

        Ok(Taxonomy { categories })
    }
}

impl Taxonomy {
    /// Exact (case-insensitive) match against the live taxonomy values.
    pub fn exact_match(&self, candidate: &str) -> Option<String> {
        self.categories
            .iter()
            .find(|c| c.eq_ignore_ascii_case(candidate.trim()))
            .cloned()
    }

    /// The first present value from a preference list (e.g. safe fallbacks).
    pub fn first_present(&self, prefs: &[&str]) -> Option<String> {
        prefs.iter().find_map(|p| self.exact_match(p))
    }
}

impl GamivoidGamePatch {
    /// Builder: include the primary category in the patch.
    pub fn with_category(mut self, category: &str) -> Self {
        self.category = Some(category.to_string());
        self
    }

    /// Builder: include tags in the patch.
    pub fn with_tags(mut self, tags: &[String]) -> Self {
        self.tags = Some(tags.iter().take(30).cloned().collect());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_envelope_parses_guide_shape() {
        let text = r#"{"game":{"slug":"example-arena","published":false}}"#;
        let env: GameEnvelope = serde_json::from_str(text).unwrap();
        assert_eq!(env.game.slug, "example-arena");
        assert!(!env.game.published);
    }

    #[test]
    fn patch_serializes_guide_field_names() {
        let patch = GamivoidGamePatch {
            file_size: Some("2.4 GB".into()),
            download_links: Some(vec![DownloadLink {
                label: "Download game".into(),
                url: "https://dl.test/x".into(),
                platform: Some("Windows".into()),
                version: Some("1.0".into()),
                file_size: None,
                note: None,
            }]),
            seo_title: Some("T Free Download PC Game".into()),
            license_type: Some("Free to play".into()),
            ..Default::default()
        };
        let v = serde_json::to_value(&patch).unwrap();
        assert!(v.get("fileSize").is_some());
        assert!(v.get("downloadLinks").is_some());
        assert!(v.get("seoTitle").is_some());
        assert!(v.get("licenseType").is_some());
        assert!(v.get("metaTitle").is_none());
        // skipped-None fields must be absent, not null
        assert!(v["downloadLinks"][0].get("fileSize").is_none());
    }

    #[test]
    fn admin_list_parses_wrapped_and_cursor_shapes() {
        let wrapped: serde_json::Value = serde_json::from_str(
            r#"{"games":[{"slug":"a","published":true},{"slug":"b"}],"hasMore":false}"#,
        )
        .unwrap();
        let games = parse_admin_games(&wrapped);
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].slug, "a");
        assert!(games[0].published);
        assert!(!games[1].published);
        assert_eq!(parse_cursor(&wrapped), Some((false, None)));
    }

    #[test]
    fn taxonomy_exact_match_is_case_insensitive() {
        let tax = Taxonomy {
            categories: vec!["Action".into(), "RPG".into()],
        };
        assert_eq!(tax.exact_match("action"), Some("Action".into()));
        assert_eq!(tax.exact_match("Action"), Some("Action".into()));
        assert_eq!(tax.exact_match("Shooter"), None);
        assert_eq!(tax.first_present(&["Shooter", "RPG"]), Some("RPG".into()));
    }

    #[test]
    fn index_entry_verified_defaults_false() {
        let text = r#"{"slug":"x","last_version":null,"last_download_source":null}"#;
        let entry: IndexEntry = serde_json::from_str(text).unwrap();
        assert!(!entry.verified);
    }
}
