use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub gamivoid_api_base: String,
    pub gamivoid_publishing_key: String,
    #[serde(default)]
    pub steam_api_key: Option<String>,
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
