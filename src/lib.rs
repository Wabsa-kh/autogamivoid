pub mod client;
pub mod client_gamivoid;
pub mod config;
pub mod downloads;
pub mod enrichment;
pub mod index;
pub mod matching;
pub mod models;
pub mod scraper;
pub mod seo;
pub mod slug;
pub mod workflow;

pub use client_gamivoid::GamivoidClient;
pub use config::Config;
pub use models::{EnrichedGame, GamivoidGamePatch};
pub use workflow::{run, run_action, RunAction, RunSummary};
