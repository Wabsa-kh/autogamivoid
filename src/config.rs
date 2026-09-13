use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub gamivoid_api_base: String,
    pub gamivoid_publishing_key: String,
    #[serde(default)]
    pub steam_api_key: Option<String>,
    /// SteamSpy API base for real Steam user tags (override only for testing).
    #[serde(default)]
    pub steamspy_api: Option<String>,
    /// Steam storefront base URL (override only for testing).
    #[serde(default)]
    pub steam_store_api: Option<String>,
    pub sources: Sources,
    #[serde(default)]
    pub batch: BatchConfig,
    #[serde(default)]
    pub state_path: Option<PathBuf>,
    /// Site name used in SEO copy (defaults to "Gamivoid").
    #[serde(default)]
    pub site_name: Option<String>,
    /// Where to export the published-games manifest consumed by the
    /// update-check workflow (defaults to state.json's sibling
    /// published-games.json).
    #[serde(default)]
    pub published_manifest_path: Option<PathBuf>,
    /// How listings are created: "draft" (default; published: false so you
    /// can review) or "publish" (listings passing all publish gates go live
    /// immediately).
    #[serde(default)]
    pub publish_mode: PublishMode,
    /// When true, no writes are made to gamivoid; the run validates and logs only.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Sources {
    pub steamrip: SourceEndpoint,
    pub steamunlocked: SourceEndpoint,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SourceEndpoint {
    pub base_url: String,
    #[serde(default)]
    pub headers: Option<std::collections::HashMap<String, String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BatchConfig {
    /// Maximum number of publish actions in one scheduled run before resuming next time.
    #[serde(default = "default_batch_action_limit")]
    pub action_limit: usize,
    /// Rough wall-clock budget per run in seconds (0 = unlimited).
    #[serde(default = "default_batch_seconds")]
    pub seconds_limit: u64,
    /// Pause between gamivoid owner API requests, in milliseconds.
    #[serde(default = "default_request_pause_ms")]
    pub request_pause_ms: u64,
}

fn default_batch_action_limit() -> usize {
    200
}

fn default_batch_seconds() -> u64 {
    2700
}

fn default_request_pause_ms() -> u64 {
    250
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            action_limit: default_batch_action_limit(),
            seconds_limit: default_batch_seconds(),
            request_pause_ms: default_request_pause_ms(),
        }
    }
}

/// Publishing mode: drafts first (safe) or direct publish.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PublishMode {
    /// Create every listing as `published: false`; review in the dashboard,
    /// then publish there or switch the mode.
    #[default]
    Draft,
    /// Listings passing every publish gate get `published: true` right away;
    /// anything incomplete stays a draft.
    Publish,
}

impl PublishMode {
    pub fn parse(s: &str) -> Option<PublishMode> {
        match s.to_lowercase().as_str() {
            "draft" => Some(PublishMode::Draft),
            "publish" | "published" | "live" => Some(PublishMode::Publish),
            _ => None,
        }
    }

    pub fn is_publish(&self) -> bool {
        matches!(self, PublishMode::Publish)
    }
}

#[cfg(test)]
mod publish_mode_tests {
    use super::*;

    #[test]
    fn parses_modes() {
        assert_eq!(PublishMode::parse("draft"), Some(PublishMode::Draft));
        assert_eq!(PublishMode::parse("Publish"), Some(PublishMode::Publish));
        assert_eq!(PublishMode::parse("nope"), None);
        assert!(!PublishMode::default().is_publish());
        assert!(PublishMode::Publish.is_publish());
    }
}
