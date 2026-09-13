use anyhow::Result;
use autogamivoid::{run_action, Config, RunAction};
use std::env;
use std::path::Path;
use tracing::info;
use tracing_subscriber::fmt;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "autogamivoid=info".into()),
        )
        .init();

    let mut dry_run_override = false;
    let mut action: Option<RunAction> = None;
    let mut config_path: Option<String> = None;

    let mut args = env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dry-run" => dry_run_override = true,
            "--action" => {
                let value = args.next().ok_or_else(|| {
                    anyhow::anyhow!("--action requires sync|catalog|updates|reconcile|reset")
                })?;
                action = Some(
                    RunAction::parse(&value).ok_or_else(|| {
                        anyhow::anyhow!(
                            "unknown action '{value}' (use sync|catalog|updates|reconcile|reset)"
                        )
                    })?,
                );
            }
            other => config_path = Some(other.to_string()),
        }
    }

    let config_path = config_path
        .or_else(|| env::var_os("AUTOGAMIVOID_CONFIG").map(|p| p.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "autogamivoid/config.json".to_string());

    info!("Loading config from: {config_path}");
    let text = std::fs::read_to_string(Path::new(&config_path))?;
    let mut config: Config = serde_json::from_str(&text)?;

    if dry_run_override {
        config.dry_run = true;
        info!("--dry-run flag set; no writes will be made");
    }

    if !config.dry_run && action != Some(RunAction::Reconcile) {
        if config.gamivoid_api_base.is_empty() {
            anyhow::bail!("gamivoid_api_base must be configured (or use --dry-run)");
        }
        if config.gamivoid_publishing_key.is_empty() {
            anyhow::bail!("gamivoid_publishing_key must be configured (or use --dry-run)");
        }
    }

    let action = action.unwrap_or(RunAction::Sync);
    info!("Action: {action:?}");
    let summary = tokio_block_on(async { run_action(config, action).await })?;
    info!(
        "Summary: created={} updated={} unchanged={} invalid={} errors={} would_publish={} published={} reconciled={} elapsed={:.1}s",
        summary.created,
        summary.updated,
        summary.unchanged,
        summary.invalid,
        summary.errors,
        summary.dry_run_would_publish,
        summary.published,
        summary.reconciled,
        summary.elapsed_seconds,
    );

    // Non-zero exit when the run had errors so CI/Actions surfaces it.
    if summary.errors > 0 {
        anyhow::bail!("run completed with {} errors", summary.errors);
    }
    Ok(())
}

fn tokio_block_on<F, T>(fut: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(fut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrips_through_json() {
        let config = Config::default();
        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(config.gamivoid_api_base, parsed.gamivoid_api_base);
        assert!(!parsed.dry_run);
        assert_eq!(parsed.publish_mode, Default::default());
    }

    #[test]
    fn example_config_parses() {
        let text = include_str!("../config.example.json");
        let parsed: Result<Config, _> = serde_json::from_str(text);
        assert!(parsed.is_ok(), "config.example.json should parse: {parsed:?}");
    }
}
