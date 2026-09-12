use crate::client_gamivoid::GamivoidClient;
use crate::config::Config;
use crate::downloads::{choose_download, downloads_to_patch_field};
use crate::enrichment::{build_enriched, EnrichmentInput};
use crate::index::Index;
use crate::matching::match_sources;
use crate::models::{EnrichedGame, Taxonomy};
use crate::scraper::{
    scrape_catalog_steamrip, scrape_catalog_steamunlocked, scrape_steamrip, scrape_steamunlocked,
};
use std::time::Instant;
use tracing::{debug, info, warn};

/// What a run should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunAction {
    /// Scrape the sites' front/recent listings and publish new + changed games.
    Sync,
    /// Walk the A-Z catalog hubs and import every game, one by one.
    Catalog,
    /// Re-check every published game (from state.json) against the sources and
    /// PATCH only the ones whose download/version changed.
    Updates,
}

impl RunAction {
    pub fn parse(s: &str) -> Option<RunAction> {
        match s.to_lowercase().as_str() {
            "sync" => Some(RunAction::Sync),
            "catalog" => Some(RunAction::Catalog),
            "updates" => Some(RunAction::Updates),
            _ => None,
        }
    }
}

/// Backwards-compatible entrypoint: a plain sync run.
pub async fn run(config: Config) -> anyhow::Result<RunSummary> {
    run_action(config, RunAction::Sync).await
}

pub async fn run_action(config: Config, action: RunAction) -> anyhow::Result<RunSummary> {
    match action {
        RunAction::Sync => run_sync(config).await,
        RunAction::Catalog => run_catalog(config).await,
        RunAction::Updates => run_updates(config).await,
    }
}

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

struct RunContext {
    limit: usize,
    deadline: Option<Instant>,
    index: Index,
    publisher: Option<(GamivoidClient, Taxonomy)>,
}

async fn make_context(config: &Config, start: Instant) -> anyhow::Result<RunContext> {
    let dry_run = config.dry_run;
    let limit = config.batch.action_limit.max(1);
    let deadline = (config.batch.seconds_limit > 0)
        .then(|| start + std::time::Duration::from_secs(config.batch.seconds_limit));

    let index = Index::load(config.state_path.clone())?;

    let publisher = if dry_run || config.gamivoid_api_base.is_empty() {
        if !dry_run {
            warn!("gamivoid_api_base is empty; falling back to dry-run");
        }
        None
    } else {
        let client = GamivoidClient::new(
            &config.gamivoid_api_base,
            &config.gamivoid_publishing_key,
            config.batch.request_pause_ms,
        )?;
        let taxonomy = client.fetch_taxonomy().await?;
        info!(
            "Taxonomy loaded with {} categories",
            taxonomy.categories.len()
        );
        Some((client, taxonomy))
    };

    Ok(RunContext {
        limit,
        deadline,
        index,
        publisher,
    })
}

fn time_and_budget_left(ctx: &RunContext, summary: &RunSummary) -> bool {
    if summary.acted >= ctx.limit {
        info!("Reached action limit {}; will resume next run", ctx.limit);
        return false;
    }
    if let Some(deadline) = ctx.deadline {
        if Instant::now() >= deadline {
            info!("Reached time budget; will resume next run");
            return false;
        }
    }
    true
}

async fn fresh_matches(config: &Config) -> anyhow::Result<Vec<crate::models::MatchedGame>> {
    let sr = scrape_steamrip(&config.sources.steamrip).await;
    let su = scrape_steamunlocked(&config.sources.steamunlocked).await;

    // One source failing should not kill the run if the other produced data.
    let (sr, su) = match (sr, su) {
        (Ok(a), Ok(b)) => (a, b),
        (Ok(a), Err(e)) => {
            warn!("steamunlocked scrape failed, continuing with steamrip only: {e:#}");
            (a, Vec::new())
        }
        (Err(e), Ok(b)) => {
            warn!("steamrip scrape failed, continuing with steamunlocked only: {e:#}");
            (Vec::new(), b)
        }
        (Err(ea), Err(eb)) => {
            anyhow::bail!("both source scrapes failed: steamrip: {ea:#}; steamunlocked: {eb:#}");
        }
    };

    info!(
        "Scraped {} steamrip + {} steamunlocked entries",
        sr.len(),
        su.len()
    );
    Ok(match_sources(sr, su))
}

async fn enrich_game(game: &crate::models::MatchedGame, config: &Config) -> Option<EnrichedGame> {
    let best = choose_download(
        &game
            .candidates
            .iter()
            .cloned()
            .map(Into::into)
            .collect::<Vec<_>>(),
    );
    let candidates: Vec<crate::models::DownloadCandidate> = game
        .candidates
        .iter()
        .cloned()
        .map(Into::into)
        .collect();

    match build_enriched(&EnrichmentInput {
        normalized_title: game.normalized_title.clone(),
        raw_title: game.raw_title.clone(),
        best_download: best.clone(),
        candidates,
        steam_app_id: None,
        steam_store_api: config.steam_store_api.clone(),
        // HEAD-verify images against Steam's CDN so no listing ships a dead URL.
        verify_images: true,
    }) {
        Ok(e) => Some(e),
        Err(e) => {
            warn!(
                "Enrichment failed for {}: {e:#}",
                game.normalized_title
            );
            None
        }
    }
}

/// Finish a run: persist state and export the published-games manifest.
fn finish(config: &Config, ctx: &mut RunContext, summary: &mut RunSummary, start: Instant) -> anyhow::Result<()> {
    ctx.index.save()?;
    export_manifest(config, ctx)?;
    summary.elapsed_seconds = start.elapsed().as_secs_f64();
    info!(
        "Run finished in {:.1}s: created={} updated={} unchanged={} invalid={} errors={} dry_run_would_publish={}",
        summary.elapsed_seconds,
        summary.created,
        summary.updated,
        summary.unchanged,
        summary.invalid,
        summary.errors,
        summary.dry_run_would_publish,
    );
    Ok(())
}

fn export_manifest(config: &Config, ctx: &RunContext) -> anyhow::Result<()> {
    let Some(state_path) = &config.state_path else {
        return Ok(());
    };
    let manifest_path = config.published_manifest_path.clone().unwrap_or_else(|| {
        state_path
            .with_file_name("published-games.json")
    });
    let rows = ctx.index.manifest();
    let text = serde_json::to_string_pretty(&rows)?;
    let tmp = manifest_path.with_extension("tmp");
    std::fs::write(&tmp, &text)?;
    std::fs::rename(&tmp, &manifest_path)?;
    info!(
        "Exported published-games manifest: {} ({} games)",
        manifest_path.display(),
        rows.len()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Sync run: fresh listings -> publish new/changed
// ---------------------------------------------------------------------------

async fn run_sync(config: Config) -> anyhow::Result<RunSummary> {
    let start = Instant::now();
    info!(
        "Starting autogamivoid SYNC run (dry_run={}, action_limit={})",
        config.dry_run, config.batch.action_limit
    );
    let mut ctx = make_context(&config, start).await?;
    let mut summary = RunSummary::default();

    let games = fresh_matches(&config).await?;
    info!("Matched {} games from sources", games.len());

    for game in &games {
        if !time_and_budget_left(&ctx, &summary) {
            break;
        }

        let slug = crate::slug::slug_from_title(&game.raw_title);
        let best = choose_download(
            &game
                .candidates
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        );

        // Skip games whose published download is unchanged.
        if !ctx.index.has_changes(&slug, best.version.as_deref(), &best.source_label) {
            debug!("Skipping unchanged game: {slug}");
            summary.unchanged += 1;
            continue;
        }

        let Some(enriched) = enrich_game(game, &config).await else {
            summary.errors += 1;
            continue;
        };

        let existed = ctx.index.entries.contains_key(&slug);

        match &ctx.publisher {
            Some((client, taxonomy)) => {
                match publish_game(client, taxonomy, &slug, &enriched, existed).await {
                    Ok(()) => {
                        if existed {
                            summary.updated += 1;
                        } else {
                            summary.created += 1;
                        }
                        summary.acted += 1;
                    }
                    Err(e) => {
                        warn!("Publish failed for {slug}: {e:#}");
                        summary.errors += 1;
                        continue;
                    }
                }
            }
            None => {
                if let Err(e) = validate_minimum(&enriched, taxonomy_fallback()) {
                    debug!("Dry-run validation failed for {slug}: {e}");
                    summary.invalid += 1;
                    continue;
                }
                debug!("Dry-run: would publish {slug}");
                summary.dry_run_would_publish += 1;
                summary.acted += 1;
            }
        }

        ctx.index.remember(
            &slug,
            Some(enriched.raw_title.trim().to_string()),
            best.version.clone(),
            Some(best.source_label.clone()),
        );
        ctx.index.checkpoint(&slug);
    }

    finish(&config, &mut ctx, &mut summary, start)?;
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Catalog run: walk A-Z hubs and import every game, one by one
// ---------------------------------------------------------------------------

async fn run_catalog(config: Config) -> anyhow::Result<RunSummary> {
    let start = Instant::now();
    info!(
        "Starting autogamivoid CATALOG run (dry_run={}, action_limit={})",
        config.dry_run, config.batch.action_limit
    );
    let mut ctx = make_context(&config, start).await?;
    let mut summary = RunSummary::default();

    info!("Fetching full A-Z catalogs from both sources");
    let sr = scrape_catalog_steamrip(&config.sources.steamrip).await;
    let su = scrape_catalog_steamunlocked(&config.sources.steamunlocked).await;
    let (sr, su) = match (sr, su) {
        (Ok(a), Ok(b)) => (a, b),
        (Ok(a), Err(e)) => {
            warn!("steamunlocked catalog failed, continuing with steamrip only: {e:#}");
            (a, Vec::new())
        }
        (Err(e), Ok(b)) => {
            warn!("steamrip catalog failed, continuing with steamunlocked only: {e:#}");
            (Vec::new(), b)
        }
        (Err(ea), Err(eb)) => {
            anyhow::bail!("both catalog scrapes failed: steamrip: {ea:#}; steamunlocked: {eb:#}");
        }
    };
    info!(
        "Catalog sizes: {} steamrip + {} steamunlocked",
        sr.len(),
        su.len()
    );

    let games = match_sources(sr, su);
    info!("Catalog matched into {} unique games", games.len());

    for game in &games {
        if !time_and_budget_left(&ctx, &summary) {
            break;
        }

        let slug = crate::slug::slug_from_title(&game.raw_title);
        let best = choose_download(
            &game
                .candidates
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        );

        // state.json is the source of truth: published + unchanged -> skip.
        if !ctx.index.entries.contains_key(&slug)
            || ctx.index.has_changes(&slug, best.version.as_deref(), &best.source_label)
        {
            // New or changed: enrich and publish below.
        } else {
            debug!("Catalog skip (already published, unchanged): {slug}");
            summary.unchanged += 1;
            continue;
        }

        let Some(enriched) = enrich_game(game, &config).await else {
            summary.errors += 1;
            continue;
        };

        let existed = ctx.index.entries.contains_key(&slug);

        match &ctx.publisher {
            Some((client, taxonomy)) => {
                match publish_game(client, taxonomy, &slug, &enriched, existed).await {
                    Ok(()) => {
                        if existed {
                            summary.updated += 1;
                        } else {
                            summary.created += 1;
                        }
                        summary.acted += 1;
                    }
                    Err(e) => {
                        if !existed && is_conflict(&e) {
                            // State was lost but the listing exists: patch instead.
                            info!("Create conflicted for {slug}; patching existing listing");
                            let patch = crate::models::GamivoidGamePatch {
                                downloads: Some(enriched.downloads.clone()),
                                ..Default::default()
                            };
                            match client.patch_game(&slug, &patch).await {
                                Ok(()) => {
                                    summary.updated += 1;
                                    summary.acted += 1;
                                }
                                Err(pe) => {
                                    warn!("Conflict-patch failed for {slug}: {pe:#}");
                                    summary.errors += 1;
                                    continue;
                                }
                            }
                        } else {
                            warn!("Publish failed for {slug}: {e:#}");
                            summary.errors += 1;
                            continue;
                        }
                    }
                }
            }
            None => {
                if let Err(e) = validate_minimum(&enriched, taxonomy_fallback()) {
                    debug!("Dry-run validation failed for {slug}: {e}");
                    summary.invalid += 1;
                    continue;
                }
                debug!("Dry-run: would publish {slug}");
                summary.dry_run_would_publish += 1;
                summary.acted += 1;
            }
        }

        ctx.index.remember(
            &slug,
            Some(enriched.raw_title.trim().to_string()),
            best.version.clone(),
            Some(best.source_label.clone()),
        );
        ctx.index.checkpoint(&slug);
    }

    finish(&config, &mut ctx, &mut summary, start)?;
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Updates run: re-check the published list for changed downloads
// ---------------------------------------------------------------------------

async fn run_updates(config: Config) -> anyhow::Result<RunSummary> {
    let start = Instant::now();
    info!(
        "Starting autogamivoid UPDATES run (dry_run={}, published={})",
        config.dry_run,
        Index::load(config.state_path.clone())?.entries.len()
    );
    let mut ctx = make_context(&config, start).await?;
    let published_slugs: Vec<String> = ctx.index.entries.keys().cloned().collect();
    let mut summary = RunSummary::default();

    if published_slugs.is_empty() {
        warn!("No published games in state; run catalog or sync first");
        finish(&config, &mut ctx, &mut summary, start)?;
        return Ok(summary);
    }

    let games = fresh_matches(&config).await?;
    let by_slug: std::collections::HashMap<String, &crate::models::MatchedGame> = games
        .iter()
        .map(|g| (crate::slug::slug_from_title(&g.raw_title), g))
        .collect();

    let mut checked = 0usize;
    let mut missing_from_sources = 0usize;

    for slug in &published_slugs {
        if !time_and_budget_left(&ctx, &summary) {
            break;
        }
        checked += 1;

        let Some(game) = by_slug.get(slug) else {
            missing_from_sources += 1;
            continue;
        };

        let best = choose_download(
            &game
                .candidates
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        );
        if !ctx.index.has_changes(slug, best.version.as_deref(), &best.source_label) {
            summary.unchanged += 1;
            continue;
        }

        info!("Update found for {slug}: version {:?} via {}", best.version, best.source_label);
        let Some(enriched) = enrich_game(game, &config).await else {
            summary.errors += 1;
            continue;
        };

        match &ctx.publisher {
            Some((client, taxonomy)) => {
                match publish_game(client, taxonomy, slug, &enriched, true).await {
                    Ok(()) => {
                        summary.updated += 1;
                        summary.acted += 1;
                    }
                    Err(e) => {
                        warn!("Update publish failed for {slug}: {e:#}");
                        summary.errors += 1;
                        continue;
                    }
                }
            }
            None => {
                if let Err(e) = validate_minimum(&enriched, taxonomy_fallback()) {
                    debug!("Dry-run validation failed for {slug}: {e}");
                    summary.invalid += 1;
                    continue;
                }
                debug!("Dry-run: would update {slug}");
                summary.dry_run_would_publish += 1;
                summary.acted += 1;
            }
        }

        let entry = ctx.index.entries.get_mut(slug);
        if let Some(entry) = entry {
            entry.last_version = best.version.clone();
            entry.last_download_source = Some(best.source_label.clone());
        }
        ctx.index.checkpoint(slug);
    }

    info!(
        "Updates scan: checked={checked} missing_from_sources={missing_from_sources}",
    );

    finish(&config, &mut ctx, &mut summary, start)?;
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Publishing
// ---------------------------------------------------------------------------

async fn publish_game(
    client: &GamivoidClient,
    taxonomy: &Taxonomy,
    slug: &str,
    enriched: &EnrichedGame,
    existed: bool,
) -> anyhow::Result<()> {
    validate_minimum(enriched, taxonomy)?;

    if existed {
        // Only refresh the download set; keep the rest of the listing untouched.
        let patch = crate::models::GamivoidGamePatch {
            downloads: Some(enriched.downloads.clone()),
            ..Default::default()
        };
        client.patch_game(slug, &patch).await
    } else {
        let body = game_create_payload(enriched);
        client.create_game(&body).await.map(|_| ())
    }
}

fn game_create_payload(enriched: &EnrichedGame) -> serde_json::Value {
    let slug = crate::slug::slug_from_title(&enriched.raw_title);
    let mut map = serde_json::Map::new();
    map.insert("slug".into(), serde_json::Value::String(slug));
    map.insert(
        "title".into(),
        serde_json::Value::String(enriched.raw_title.trim().to_string()),
    );
    map.insert(
        "description".into(),
        serde_json::Value::String(enriched.description.clone()),
    );

    if let Some(dev) = &enriched.developer {
        map.insert("developer".into(), serde_json::Value::String(dev.clone()));
    }
    if let Some(publ) = &enriched.publisher {
        map.insert("publisher".into(), serde_json::Value::String(publ.clone()));
    }
    if let Some(date) = &enriched.release_date {
        map.insert("releaseDate".into(), serde_json::Value::String(date.clone()));
    }
    if !enriched.platforms.is_empty() {
        map.insert(
            "platforms".into(),
            serde_json::to_value(&enriched.platforms).unwrap_or(serde_json::Value::Null),
        );
    }
    if !enriched.tags.is_empty() {
        map.insert(
            "tags".into(),
            serde_json::to_value(&enriched.tags).unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(cat) = &enriched.category {
        map.insert("category".into(), serde_json::Value::String(cat.clone()));
    }
    if let Some(cover) = &enriched.cover_image_url {
        map.insert("coverImage".into(), serde_json::Value::String(cover.clone()));
    }
    if let Some(hero) = &enriched.hero_image_url {
        map.insert("heroImage".into(), serde_json::Value::String(hero.clone()));
    }
    if !enriched.screenshot_urls.is_empty() {
        map.insert(
            "screenshotUrls".into(),
            serde_json::to_value(&enriched.screenshot_urls).unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(url) = &enriched.steam_store_url {
        map.insert("steamStoreUrl".into(), serde_json::Value::String(url.clone()));
    }
    // SEO fields — ignored gracefully by APIs that don't know them.
    if !enriched.meta_title.is_empty() {
        map.insert("metaTitle".into(), serde_json::Value::String(enriched.meta_title.clone()));
    }
    if !enriched.meta_description.is_empty() {
        map.insert(
            "metaDescription".into(),
            serde_json::Value::String(enriched.meta_description.clone()),
        );
    }
    if !enriched.meta_keywords.is_empty() {
        map.insert(
            "metaKeywords".into(),
            serde_json::to_value(&enriched.meta_keywords).unwrap_or(serde_json::Value::Null),
        );
    }
    map.insert(
        "downloads".into(),
        serde_json::Value::Array(downloads_to_patch_field(&enriched.downloads)),
    );

    serde_json::Value::Object(map)
}

/// In dry-run mode there is no live taxonomy; validation only checks intrinsic fields.
fn taxonomy_fallback() -> &'static Taxonomy {
    use std::sync::OnceLock;
    static FALLBACK: OnceLock<Taxonomy> = OnceLock::new();
    FALLBACK.get_or_init(|| Taxonomy {
        categories: Vec::new(),
    })
}

fn validate_minimum(
    enriched: &EnrichedGame,
    taxonomy: &Taxonomy,
) -> anyhow::Result<()> {
    anyhow::ensure!(!enriched.raw_title.trim().is_empty(), "empty title");
    anyhow::ensure!(!enriched.description.trim().is_empty(), "empty description");
    anyhow::ensure!(
        enriched.cover_image_url.is_some() || enriched.hero_image_url.is_some(),
        "missing cover/hero image URL"
    );
    for download in &enriched.downloads {
        anyhow::ensure!(
            download.url.starts_with("http://") || download.url.starts_with("https://"),
            "download URL is not http(s): {}",
            download.url
        );
    }
    if let Some(cat) = &enriched.category {
        if !taxonomy.categories.is_empty() {
            anyhow::ensure!(
                taxonomy
                    .categories
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(cat)),
                "category '{cat}' is not in taxonomy"
            );
        }
    }
    Ok(())
}

fn is_conflict(err: &anyhow::Error) -> bool {
    let text = err.to_string();
    text.contains("409") || text.to_lowercase().contains("already exists")
}

#[derive(Debug, Default, Clone)]
pub struct RunSummary {
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub invalid: usize,
    pub errors: usize,
    pub dry_run_would_publish: usize,
    pub acted: usize,
    pub elapsed_seconds: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SourceLabel;

    #[test]
    fn action_parses_all_modes() {
        assert_eq!(RunAction::parse("sync"), Some(RunAction::Sync));
        assert_eq!(RunAction::parse("Catalog"), Some(RunAction::Catalog));
        assert_eq!(RunAction::parse("UPDATES"), Some(RunAction::Updates));
        assert_eq!(RunAction::parse("nonsense"), None);
    }

    #[test]
    fn create_payload_contains_slug_title_seo_and_downloads() {
        let enriched = EnrichedGame {
            normalized_title: "doom".into(),
            raw_title: "DOOM".into(),
            description: "<p>A game.</p>".into(),
            developer: Some("id".into()),
            publisher: None,
            release_date: None,
            platforms: vec!["Windows".into()],
            tags: vec!["Action".into()],
            category: Some("Action".into()),
            cover_image_url: Some("https://img.test/cover.jpg".into()),
            hero_image_url: None,
            screenshot_urls: vec![],
            downloads: vec![crate::models::DownloadCandidate {
                url: "https://dl.test/doom".into(),
                source_label: SourceLabel::Steamrip,
                version: Some("1.0".into()),
                notes: None,
            }],
            steam_store_url: None,
            meta_title: "DOOM Free Download PC (v1.0)".into(),
            meta_description: "Download DOOM for free on PC.".into(),
            meta_keywords: vec!["doom free download".into()],
            published_at: None,
        };

        let payload = game_create_payload(&enriched);
        assert_eq!(payload["slug"], "doom");
        assert_eq!(payload["title"], "DOOM");
        assert_eq!(payload["category"], "Action");
        assert_eq!(payload["downloads"][0]["url"], "https://dl.test/doom");
        assert_eq!(payload["downloads"][0]["source"], "steamrip");
        assert_eq!(payload["metaTitle"], "DOOM Free Download PC (v1.0)");
        assert_eq!(payload["metaDescription"], "Download DOOM for free on PC.");
        assert!(payload["publisher"].is_null());
    }

    #[test]
    fn validation_rejects_missing_images() {
        let enriched = EnrichedGame {
            normalized_title: "x".into(),
            raw_title: "X".into(),
            description: "d".into(),
            developer: None,
            publisher: None,
            release_date: None,
            platforms: vec![],
            tags: vec![],
            category: None,
            cover_image_url: None,
            hero_image_url: None,
            screenshot_urls: vec![],
            downloads: vec![],
            steam_store_url: None,
            meta_title: String::new(),
            meta_description: String::new(),
            meta_keywords: vec![],
            published_at: None,
        };
        let tax = Taxonomy {
            categories: vec![],
        };
        assert!(validate_minimum(&enriched, &tax).is_err());
    }
}
