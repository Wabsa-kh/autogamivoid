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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnrichedGame {
    pub normalized_title: String,
    pub raw_title: String,
    pub description: String,
    pub developer: Option<String>,
    pub publisher: Option<String>,
    pub release_date: Option<String>,
    pub platforms: Vec<String>,
    pub tags: Vec<String>,
    pub category: Option<String>,
    pub cover_image_url: Option<String>,
    pub hero_image_url: Option<String>,
    pub screenshot_urls: Vec<String>,
    pub downloads: Vec<DownloadCandidate>,
    pub steam_store_url: Option<String>,
    /// SEO: `<title>`-style string, kept short (~60 chars).
    pub meta_title: String,
    /// SEO: meta description, kept under ~155 chars.
    pub meta_description: String,
    /// SEO: keyword list for meta keywords / tag clouds.
    pub meta_keywords: Vec<String>,
    /// RFC3339 timestamp set by the workflow when the listing is published.
    pub published_at: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GamivoidGamePatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub developer: Option<String>,
    pub publisher: Option<String>,
    pub release_date: Option<String>,
    pub platforms: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub category: Option<String>,
    pub cover_image: Option<String>,
    pub hero_image: Option<String>,
    pub screenshot_urls: Option<Vec<String>>,
    pub downloads: Option<Vec<DownloadCandidate>>,
    pub steam_store_url: Option<String>,
    pub meta_title: Option<String>,
    pub meta_description: Option<String>,
    pub meta_keywords: Option<Vec<String>>,
    pub published_at: Option<String>,
}

impl GamivoidGamePatch {
    #[allow(clippy::option_option)]
    pub fn all_fields_none(&self) -> bool {
        self.title.is_none()
            && self.description.is_none()
            && self.developer.is_none()
            && self.publisher.is_none()
            && self.release_date.is_none()
            && self.platforms.is_none()
            && self.tags.is_none()
            && self.category.is_none()
            && self.cover_image.is_none()
            && self.hero_image.is_none()
            && self.screenshot_urls.is_none()
            && self.downloads.is_none()
            && self.steam_store_url.is_none()
            && self.meta_title.is_none()
            && self.meta_description.is_none()
            && self.meta_keywords.is_none()
            && self.published_at.is_none()
    }
}

/// Minimal shape of the gamivoid API create-game response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GamivoidPublishedGame {
    pub slug: String,
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
