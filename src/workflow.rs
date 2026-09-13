use crate::client_gamivoid::GamivoidClient;
use crate::config::Config;
use crate::downloads::choose_download;
use crate::enrichment::{build_enriched, EnrichmentInput};
use crate::index::Index;
use crate::matching::match_sources;
use crate::models::{EnrichedGame, GamivoidGamePatch, Taxonomy};
use crate::scraper::{
    scrape_catalog_steamrip, scrape_catalog_steamunlocked, scrape_steamrip, scrape_steamunlocked,
};
use std::time::Instant;
use tracing::{info, warn};

/// What a run should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunAction {
    /// Scrape the sites' front/recent listings and publish new + changed games.
    Sync,
    /// Walk the A-Z catalog hubs and import every game, one by one.
    Catalog,
    /// Re-check every published game (from state.json) against the sources and
    /// update only the ones whose download/version changed.
    Updates,
    /// Inventory owner listings on the site and rebuild state.json from what
    /// actually exists (heals phantoms from old dry-runs, recovers the drafts
    /// created by pre-guide runs).
    Reconcile,
}

impl RunAction {
    pub fn parse(s: &str) -> Option<RunAction> {
        match s.to_lowercase().as_str() {
            "sync" => Some(RunAction::Sync),
            "catalog" => Some(RunAction::Catalog),
            "updates" => Some(RunAction::Updates),
            "reconcile" => Some(RunAction::Reconcile),
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
        RunAction::Reconcile => run_reconcile(config).await,
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
    /// True when nothing may be written anywhere (dry-run or missing API).
    dry_run: bool,
}

async fn make_context(config: &Config, start: Instant) -> anyhow::Result<RunContext> {
    let limit = config.batch.action_limit.max(1);
    let deadline = (config.batch.seconds_limit > 0)
        .then(|| start + std::time::Duration::from_secs(config.batch.seconds_limit));

    let index = Index::load(config.state_path.clone())?;

    let publisher = if config.dry_run || config.gamivoid_api_base.is_empty() {
        if !config.dry_run {
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
        info!("Taxonomy loaded with {} categories", taxonomy.categories.len());
        Some((client, taxonomy))
    };

    Ok(RunContext {
        limit,
        deadline,
        index,
        dry_run: publisher.is_none(),
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

    info!("Scraped {} steamrip + {} steamunlocked entries", sr.len(), su.len());
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
        steamspy_api: config.steamspy_api.clone(),
        // HEAD-verify images against Steam's CDN so no listing ships a dead URL.
        verify_images: true,
    }) {
        Ok(e) => Some(e),
        Err(e) => {
            warn!("Enrichment failed for {}: {e:#}", game.normalized_title);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Sync run: fresh listings -> publish new/changed
// ---------------------------------------------------------------------------

async fn run_sync(config: Config) -> anyhow::Result<RunSummary> {
    let start = Instant::now();
    info!(
        "Starting autogamivoid SYNC run (dry_run={}, publish_mode={:?}, action_limit={})",
        config.dry_run, config.publish_mode, config.batch.action_limit
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

        // Skip unchanged games only when the entry is verified on the site.
        // Unverified entries (dry-run phantoms, pre-guide state) fall through
        // and get republished, healing the state automatically.
        if let Some(entry) = ctx.index.entries.get(&slug) {
            if entry.verified && !ctx.index.has_changes(&slug, best.version.as_deref(), &best.source_label) {
                summary.unchanged += 1;
                continue;
            }
        }

        let Some(enriched) = enrich_game(game, &config).await else {
            summary.errors += 1;
            continue;
        };

        let existed = ctx
            .index
            .entries
            .get(&slug)
            .map(|e| e.verified)
            .unwrap_or(false);

        process_listing(&mut ctx, &config, &slug, &enriched, existed, &mut summary)
            .await;

        if !ctx.dry_run {
            ctx.index.remember_verified(
                &slug,
                Some(enriched.raw_title.trim().to_string()),
                best.version.clone(),
                Some(best.source_label.clone()),
            );
            ctx.index.checkpoint(&slug);
        }
    }

    finish(&mut ctx, &mut summary, start)?;
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Catalog run: walk A-Z hubs and import every game, one by one
// ---------------------------------------------------------------------------

async fn run_catalog(config: Config) -> anyhow::Result<RunSummary> {
    let start = Instant::now();
    info!(
        "Starting autogamivoid CATALOG run (dry_run={}, publish_mode={:?}, action_limit={})",
        config.dry_run, config.publish_mode, config.batch.action_limit
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
    info!("Catalog sizes: {} steamrip + {} steamunlocked", sr.len(), su.len());

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

        // state.json is the source of truth, but only verified entries count
        // as published; unverified ones are republished (heals old phantoms).
        if let Some(entry) = ctx.index.entries.get(&slug) {
            if entry.verified && !ctx.index.has_changes(&slug, best.version.as_deref(), &best.source_label) {
                summary.unchanged += 1;
                continue;
            }
        }

        let Some(enriched) = enrich_game(game, &config).await else {
            summary.errors += 1;
            continue;
        };

        let existed = ctx
            .index
            .entries
            .get(&slug)
            .map(|e| e.verified)
            .unwrap_or(false);

        process_listing(&mut ctx, &config, &slug, &enriched, existed, &mut summary)
            .await;

        if !ctx.dry_run {
            ctx.index.remember_verified(
                &slug,
                Some(enriched.raw_title.trim().to_string()),
                best.version.clone(),
                Some(best.source_label.clone()),
            );
            ctx.index.checkpoint(&slug);
        }
    }

    finish(&mut ctx, &mut summary, start)?;
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Updates run: re-check the published list for changed downloads
// ---------------------------------------------------------------------------

async fn run_updates(config: Config) -> anyhow::Result<RunSummary> {
    let start = Instant::now();
    let mut ctx = make_context(&config, start).await?;
    let published_count = ctx.index.entries.len();
    info!(
        "Starting autogamivoid UPDATES run (dry_run={}, publish_mode={:?}, published={})",
        config.dry_run, config.publish_mode, published_count
    );
    let published_slugs: Vec<String> = ctx.index.entries.keys().cloned().collect();
    let mut summary = RunSummary::default();

    if published_slugs.is_empty() {
        warn!("No published games in state; run catalog or sync first");
        finish(&mut ctx, &mut summary, start)?;
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
        let entry = ctx.index.entries.get(slug);
        let verified = entry.map(|e| e.verified).unwrap_or(false);
        if verified && !ctx.index.has_changes(slug, best.version.as_deref(), &best.source_label) {
            summary.unchanged += 1;
            continue;
        }

        info!(
            "Update found for {slug}: version {:?} via {}",
            best.version, best.source_label
        );
        let Some(enriched) = enrich_game(game, &config).await else {
            summary.errors += 1;
            continue;
        };

        process_listing(&mut ctx, &config, slug, &enriched, verified, &mut summary).await;

        if !ctx.dry_run {
            if let Some(entry) = ctx.index.entries.get_mut(slug) {
                entry.last_version = best.version.clone();
                entry.last_download_source = Some(best.source_label.clone());
                entry.verified = true;
            }
            ctx.index.checkpoint(slug);
        }
    }

    info!("Updates scan: checked={checked} missing_from_sources={missing_from_sources}");
    finish(&mut ctx, &mut summary, start)?;
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Reconcile: inventory the site and heal state.json
// ---------------------------------------------------------------------------

async fn run_reconcile(config: Config) -> anyhow::Result<RunSummary> {
    let start = Instant::now();
    let mut ctx = make_context(&config, start).await?;
    let mut summary = RunSummary::default();

    let Some((client, _)) = &ctx.publisher else {
        anyhow::bail!("reconcile requires a live API connection (no dry-run)");
    };

    info!("Reconciling: listing all owner games on the site");
    let admin_games = client.list_admin_games().await?;
    let mut published = 0usize;
    let mut drafts = 0usize;
    for game in &admin_games {
        if game.published {
            published += 1;
        } else {
            drafts += 1;
        }
        // Any listing that exists on the site is real: verify/insert it in
        // state so later runs neither skip it as phantom nor recreate it.
        ctx.index.verify_or_insert(&game.slug, game.title.clone());
        summary.reconciled += 1;
    }
    info!(
        "Site inventory: {} listings ({} published, {} drafts)",
        admin_games.len(),
        published,
        drafts
    );

    // Phantoms: state entries that no site listing confirms.
    let site_slugs: std::collections::HashSet<&str> =
        admin_games.iter().map(|g| g.slug.as_str()).collect();
    let phantoms: Vec<String> = ctx
        .index
        .entries
        .keys()
        .filter(|s| !site_slugs.contains(s.as_str()))
        .cloned()
        .collect();
    if !phantoms.is_empty() {
        warn!(
            "{} state entries have no listing on the site (dry-run phantoms); \
             resetting them so the next catalog run republishes them",
            phantoms.len()
        );
        for slug in &phantoms {
            if let Some(entry) = ctx.index.entries.get_mut(slug) {
                entry.verified = false;
            }
        }
    }

    summary.unchanged = published; // informational: real published games
    finish(&mut ctx, &mut summary, start)?;
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Publishing (guide-exact payloads)
// ---------------------------------------------------------------------------

/// Create or update one listing on the site.
///
/// Create path: POST with the full guide payload. `published: true` only in
/// publish mode AND when every publish gate passes; otherwise the listing is
/// created as a draft.
///
/// Update path: ETag PATCH. For a known-published listing only the download
/// fields change. For an unverified/draft listing the full payload is sent
/// (backfilling the incomplete pre-guide drafts).
async fn process_listing(
    ctx: &mut RunContext,
    config: &Config,
    slug: &str,
    enriched: &EnrichedGame,
    existed: bool,
    summary: &mut RunSummary,
) {
    match &ctx.publisher {
        Some((client, taxonomy)) => {
            let result = if existed {
                update_listing(client, taxonomy, slug, enriched, config).await
            } else {
                create_listing(client, taxonomy, slug, enriched, config).await
            };
            match result {
                Ok(published_now) => {
                    if existed {
                        summary.updated += 1;
                    } else {
                        summary.created += 1;
                        if published_now {
                            summary.published += 1;
                        }
                    }
                    summary.acted += 1;
                }
                Err(e) => {
                    warn!("Publish failed for {slug}: {e:#}");
                    summary.errors += 1;
                }
            }
        }
        None => {
            // Dry-run: validate intrinsically and against the taxonomy copy
            // we already fetched (publish gates need it).
            let taxonomy = ctx
                .publisher
                .as_ref()
                .map(|(_, t)| t.clone());
            match validate_publish_gates(enriched, taxonomy.as_ref().unwrap_or(&Taxonomy { categories: vec![] })) {
                Ok(()) => {
                    summary.dry_run_would_publish += 1;
                    summary.acted += 1;
                }
                Err(e) => {
                    warn!("Dry-run validation failed for {slug}: {e:#}");
                    summary.invalid += 1;
                    summary.acted += 1;
                }
            }
        }
    }
}

async fn create_listing(
    client: &GamivoidClient,
    taxonomy: &Taxonomy,
    slug: &str,
    enriched: &EnrichedGame,
    config: &Config,
) -> anyhow::Result<bool> {
    // The primary category must be an exact taxonomy value.
    let category = resolve_category(enriched, taxonomy)?;

    let publish_now = config.publish_mode.is_publish()
        && validate_publish_gates(enriched, taxonomy).is_ok();

    let body = game_create_payload(enriched, slug, &category, publish_now);
    let saved = client.create_game(&body).await?;
    Ok(saved.published)
}

async fn update_listing(
    client: &GamivoidClient,
    taxonomy: &Taxonomy,
    slug: &str,
    enriched: &EnrichedGame,
    config: &Config,
) -> anyhow::Result<bool> {
    let current = client.get_game(slug).await?;
    let Some(current) = current else {
        // State said it exists, the site disagrees: create instead.
        return create_listing(client, taxonomy, slug, enriched, config).await;
    };

    let currently_published = current
        .get("published")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Draft (or broken old draft): backfill the full listing content.
    if !currently_published {
        let category = resolve_category(enriched, taxonomy)?;
        let publish_now = config.publish_mode.is_publish()
            && validate_publish_gates(enriched, taxonomy).is_ok();
        let mut body = game_create_patch_payload(enriched, &category);
        body.published = Some(publish_now);
        client.patch_game(slug, &body).await?;
        return Ok(publish_now);
    }

    // Published listing: refresh only the download set, preserve the rest.
    let patch = GamivoidGamePatch {
        version: enriched.version.clone(),
        file_size: enriched.file_size.clone(),
        download_links: Some(enriched.download_links.clone()),
        ..Default::default()
    };
    client.patch_game(slug, &patch).await?;
    Ok(false)
}

/// Map category guesses onto exact live taxonomy values.
fn resolve_category(
    enriched: &EnrichedGame,
    taxonomy: &Taxonomy,
) -> anyhow::Result<String> {
    // Preferred order: the enrichment's best guess, then common fallbacks
    // confirmed present in the live taxonomy (430 values; Indie is in it).
    let mut candidates: Vec<String> = Vec::new();
    if let Some(guess) = &enriched.category_guess {
        candidates.push(guess.clone());
    }
    candidates.extend(enriched.category_guesses.iter().cloned());
    candidates.push("Indie".to_string());
    candidates.push("Action".to_string());
    candidates.push("Casual".to_string());

    for candidate in &candidates {
        if let Some(exact) = taxonomy.exact_match(candidate) {
            return Ok(exact);
        }
    }
    anyhow::bail!(
        "no taxonomy match for category guesses {:?}",
        candidates.first().unwrap_or(&String::new())
    );
}

/// The guide's publish validation: everything required for `published: true`.
fn validate_publish_gates(enriched: &EnrichedGame, taxonomy: &Taxonomy) -> anyhow::Result<()> {
    anyhow::ensure!(!enriched.raw_title.trim().is_empty(), "empty title");
    anyhow::ensure!(
        enriched.description.trim().chars().count() >= 30,
        "description must be 30+ characters for published listings"
    );
    anyhow::ensure!(
        enriched.cover_image_url.is_some() || enriched.hero_image_url.is_some(),
        "missing cover/hero image URL"
    );
    anyhow::ensure!(
        enriched.minimum.as_deref().map(|m| m.chars().count() >= 10).unwrap_or(false),
        "minimum requirements must be 10+ characters"
    );
    anyhow::ensure!(
        enriched.version.as_deref().map(|v| !v.trim().is_empty()).unwrap_or(false),
        "version is required for published listings"
    );
    anyhow::ensure!(
        enriched.file_size.as_deref().map(|v| !v.trim().is_empty()).unwrap_or(false),
        "fileSize is required for published listings"
    );
    anyhow::ensure!(
        enriched.storage.as_deref().map(|v| !v.trim().is_empty()).unwrap_or(false),
        "storage is required for published listings"
    );
    anyhow::ensure!(
        enriched.license_type.trim().chars().count() >= 3,
        "licenseType is required for published listings"
    );
    anyhow::ensure!(
        !enriched.download_links.is_empty(),
        "at least one downloadLinks entry is required for published listings"
    );
    for download in &enriched.download_links {
        anyhow::ensure!(
            download.url.starts_with("https://") || download.url.starts_with("http://"),
            "download URL is not http(s): {}",
            download.url
        );
        anyhow::ensure!(
            download.url.len() <= 2000,
            "download URL exceeds 2000 chars: {}",
            download.url
        );
    }
    if let Some(source) = &enriched.source_url {
        anyhow::ensure!(
            source.starts_with("https://") || source.starts_with("http://"),
            "sourceUrl is not http(s)"
        );
    }
    // Category must exist in taxonomy (checked here for gates; create path
    // enforces it via resolve_category).
    if let Some(guess) = &enriched.category_guess {
        let matchable = taxonomy.exact_match(guess).is_some()
            || matches!(guess.as_str(), "Indie" | "Action" | "Casual");
        anyhow::ensure!(matchable, "category '{guess}' not in taxonomy");
    }
    Ok(())
}

/// Full create payload exactly in the guide's field names.
fn game_create_payload(
    enriched: &EnrichedGame,
    slug: &str,
    category: &str,
    publish_now: bool,
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("title".into(), json_str(enriched.raw_title.trim()));
    map.insert("slug".into(), json_str(slug));
    map.insert("description".into(), json_str(&enriched.description));
    map.insert("article".into(), json_str(&enriched.article_md));
    map.insert("category".into(), json_str(category));
    map.insert("tags".into(), str_array(&enriched.tags, 30));
    map.insert("platforms".into(), str_array(&enriched.platforms, 4));
    if let Some(dev) = &enriched.developer {
        map.insert("developer".into(), json_str(dev));
    }
    if let Some(publ) = &enriched.publisher {
        map.insert("publisher".into(), json_str(publ));
    }
    if let Some(date) = &enriched.release_date {
        map.insert("releaseDate".into(), json_str(date));
    }
    if let Some(version) = &enriched.version {
        map.insert("version".into(), json_str(version));
    }
    if let Some(size) = &enriched.file_size {
        map.insert("fileSize".into(), json_str(size));
    }
    if let Some(storage) = &enriched.storage {
        map.insert("storage".into(), json_str(storage));
    }
    if let Some(minimum) = &enriched.minimum {
        map.insert("minimum".into(), json_str(minimum));
    }
    if let Some(recommended) = &enriched.recommended {
        map.insert("recommended".into(), json_str(recommended));
    }
    map.insert("instructions".into(), str_array(&enriched.instructions, 20));
    if let Some(cover) = &enriched.cover_image_url {
        map.insert("coverImage".into(), json_str(cover));
        if let Some(alt) = &enriched.cover_alt {
            map.insert("coverAlt".into(), json_str(alt));
        }
    }
    if let Some(hero) = &enriched.hero_image_url {
        map.insert("heroImage".into(), json_str(hero));
    }
    if let Some(featured) = &enriched.featured_image_url {
        map.insert("featuredImage".into(), json_str(featured));
        if let Some(alt) = &enriched.featured_image_alt {
            map.insert("featuredImageAlt".into(), json_str(alt));
        }
    }
    if !enriched.screenshot_urls.is_empty() {
        map.insert("screenshots".into(), str_array(&enriched.screenshot_urls, 20));
        map.insert("screenshotAlts".into(), str_array(&enriched.screenshot_alts, 20));
    }
    map.insert(
        "downloadLinks".into(),
        serde_json::Value::Array(
            enriched
                .download_links
                .iter()
                .map(|l| serde_json::to_value(l).unwrap_or(serde_json::Value::Null))
                .collect(),
        ),
    );
    if let Some(source) = &enriched.source_url {
        map.insert("sourceUrl".into(), json_str(source));
    }
    map.insert("licenseType".into(), json_str(&enriched.license_type));
    if !enriched.seo_title.is_empty() {
        map.insert("seoTitle".into(), json_str(&enriched.seo_title));
    }
    if !enriched.seo_description.is_empty() {
        map.insert("seoDescription".into(), json_str(&enriched.seo_description));
    }
    map.insert("published".into(), serde_json::Value::Bool(publish_now));
    serde_json::Value::Object(map)
}

fn str_array(items: &[String], max: usize) -> serde_json::Value {
    serde_json::Value::Array(
        items
            .iter()
            .take(max)
            .map(|s| json_str(s))
            .collect(),
    )
}

/// PATCH payload for backfilling drafts (all content fields).
fn game_create_patch_payload(enriched: &EnrichedGame, category: &str) -> GamivoidGamePatch {
    GamivoidGamePatch {
        description: Some(enriched.description.clone()),
        article: Some(enriched.article_md.clone()),
        seo_title: (!enriched.seo_title.is_empty()).then(|| enriched.seo_title.clone()),
        seo_description: (!enriched.seo_description.is_empty())
            .then(|| enriched.seo_description.clone()),
        minimum: enriched.minimum.clone(),
        recommended: enriched.recommended.clone(),
        instructions: Some(enriched.instructions.clone()),
        source_url: enriched.source_url.clone(),
        license_type: Some(enriched.license_type.clone()),
        version: enriched.version.clone(),
        file_size: enriched.file_size.clone(),
        download_links: Some(enriched.download_links.clone()),
        ..Default::default()
    }
    .with_category(category)
    .with_tags(&enriched.tags)
}

fn json_str(s: &str) -> serde_json::Value {
    serde_json::Value::String(s.to_string())
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
    pub published: usize,
    pub reconciled: usize,
    pub removed: usize,
    pub elapsed_seconds: f64,
}

// ---------------------------------------------------------------------------
// Run completion
// ---------------------------------------------------------------------------

/// Finish a run: persist state and export the manifest. Dry-runs write
/// nothing anywhere (this is what created the old phantom state entries).
fn finish(ctx: &mut RunContext, summary: &mut RunSummary, start: Instant) -> anyhow::Result<()> {
    summary.elapsed_seconds = start.elapsed().as_secs_f64();
    if ctx.dry_run {
        info!(
            "Dry-run finished in {:.1}s (no state or site writes): created={} updated={} unchanged={} invalid={} errors={} dry_run_would_publish={}",
            summary.elapsed_seconds,
            summary.created,
            summary.updated,
            summary.unchanged,
            summary.invalid,
            summary.errors,
            summary.dry_run_would_publish,
        );
        return Ok(());
    }
    ctx.index.save()?;
    export_manifest(ctx)?;
    info!(
        "Run finished in {:.1}s: created={} updated={} unchanged={} invalid={} errors={} published={} reconciled={}",
        summary.elapsed_seconds,
        summary.created,
        summary.updated,
        summary.unchanged,
        summary.invalid,
        summary.errors,
        summary.published,
        summary.reconciled,
    );
    Ok(())
}

fn export_manifest(ctx: &RunContext) -> anyhow::Result<()> {
    let rows = ctx.index.manifest();
    let path = ctx
        .index
        .manifest_path()
        .unwrap_or_else(|| std::path::PathBuf::from("published-games.json"));
    let text = serde_json::to_string_pretty(&rows)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &text)?;
    std::fs::rename(&tmp, &path)?;
    info!(
        "Exported published-games manifest: {} ({} games)",
        path.display(),
        rows.len()
    );
    Ok(())
}
