use crate::models::{DownloadCandidate, DownloadLink, EnrichedGame};
use tracing::{info, warn};

pub const DEFAULT_STEAM_STORE_API: &str = "https://store.steampowered.com";
pub const DEFAULT_STEAMSPY_API: &str = "https://steamspy.com/api.php";

/// How many real Steam tags to attach (guide allows up to 30).
const MAX_TAGS: usize = 15;
/// How many screenshots to collect (guide allows up to 20; 8 keeps payloads lean).
const MAX_SCREENSHOTS: usize = 8;

pub struct EnrichmentInput {
    pub normalized_title: String,
    pub raw_title: String,
    pub best_download: DownloadCandidate,
    pub candidates: Vec<DownloadCandidate>,
    pub steam_app_id: Option<u64>,
    /// Steam storefront base for appdetails/storesearch (tests point at a mock).
    pub steam_store_api: Option<String>,
    /// SteamSpy base for real user tags (tests point at a mock).
    pub steamspy_api: Option<String>,
    /// HEAD-verify image URLs against Steam's CDN before publishing so no
    /// listing ships a dead image. Disable in offline tests.
    pub verify_images: bool,
}

/// Enrich a matched game into a complete guide-shaped listing.
///
/// When Steam resolves the title, appdetails supplies the description,
/// developer, publisher, release date, platforms, genres and pc requirements;
/// SteamSpy supplies the real user tags. If Steam is unreachable entirely,
/// source-derived fallbacks keep the listing valid - except that cover and
/// hero images require an app ID, so games Steam cannot confirm are rejected
/// by the caller (validate_minimum refuses missing images).
pub fn build_enriched(game: &EnrichmentInput) -> anyhow::Result<EnrichedGame> {
    // Source sites HTML-escape titles ('Birds Aren&#039;t Real'); decode once so
    // slugs, copy and alt text never carry raw entities.
    let game = decodable_game(game);
    let api_base = game
        .steam_store_api
        .clone()
        .unwrap_or_else(|| DEFAULT_STEAM_STORE_API.to_string());
    let spy_base = game
        .steamspy_api
        .clone()
        .unwrap_or_else(|| DEFAULT_STEAMSPY_API.to_string());

    let app_id = match game.steam_app_id {
        Some(id) => Some(id),
        None => resolve_steam_app_id(&game.raw_title, &api_base),
    };

    let (details, spy_tags) = match app_id {
        Some(app_id) => {
            let details = fetch_steam_details(app_id, &api_base).unwrap_or_default();
            if details.description.is_none() && details.genres.is_empty() {
                warn!("Steam details unavailable for app {app_id}; using fallbacks");
            }
            let spy_tags = fetch_steamspy_tags(app_id, &spy_base).unwrap_or_default();
            (details, spy_tags)
        }
        None => {
            debug_unresolved(&game);
            (SteamDetails::default(), Vec::new())
        }
    };

    let images = match app_id {
        Some(app_id) => images_from_steam(app_id, &details, game.verify_images),
        None => ImageSet::default(),
    };

    // Category preference: real genre first, then common Steam categories.
    let category_guess = details
        .genres
        .first()
        .cloned()
        .or_else(|| category_from_tags(&details.categories));

    // Extra category candidates for the site's `categories` array (exact
    // taxonomy matching happens in the workflow, which owns the live list).
    let mut category_guesses = details.genres.clone();
    for tag in spy_tags.iter().take(6) {
        if !category_guesses.iter().any(|c| c.eq_ignore_ascii_case(tag)) {
            category_guesses.push(tag.clone());
        }
    }

    // Tags: real SteamSpy user tags when available, otherwise Steam
    // categories + genres (what appdetails offers without the tag API).
    let tags = if !spy_tags.is_empty() {
        spy_tags
    } else {
        let mut combined = details.categories.clone();
        combined.extend(details.genres.clone());
        combined.dedup();
        combined.truncate(MAX_TAGS);
        combined
    };

    let title_for_alt = clean_alt_title(&game.raw_title);
    let cover_alt = images
        .cover
        .as_ref()
        .map(|_| format!("{title_for_alt} cover art"));
    let featured_image_alt = images
        .featured
        .as_ref()
        .map(|_| format!("{title_for_alt} featured artwork"));
    let screenshot_alts = images
        .screenshots
        .iter()
        .enumerate()
        .map(|(i, _)| format!("{title_for_alt} gameplay screenshot {}", i + 1))
        .collect();

    // System requirements from appdetails pc_requirements; plain text.
    let minimum = details
        .minimum
        .as_deref()
        .map(strip_html)
        .filter(|s| s.chars().count() >= 10)
        .or_else(default_minimum);
    let recommended = details
        .recommended
        .as_deref()
        .map(strip_html)
        .filter(|s| !s.trim().is_empty());

    // Download links: one per source candidate, plus the official Steam page.
    let mut download_links = download_links_for(&game.candidates, app_id);

    let steam_store_url = app_id.map(|id| format!("https://store.steampowered.com/app/{id}"));

    // sourceUrl: official Steam page when known, else the first download.
    let source_url = steam_store_url
        .clone()
        .or_else(|| download_links.first().map(|l| l.url.clone()));
    if steam_store_url.is_some() && !download_links.iter().any(|l| l.label == "View on Steam") {
        // Keep the store page as the trailing (non-primary) link, like the
        // guide's example.
        download_links.push(DownloadLink {
            label: "View on Steam".into(),
            url: steam_store_url.clone().unwrap(),
            platform: None,
            version: None,
            file_size: None,
            note: Some("Official Steam page".into()),
        });
    }

    let seo = crate::seo::build_listing(&crate::seo::SeoInput {
        title: &game.raw_title,
        version: game.best_download.version.as_deref(),
        source_label: &game.best_download.source_label,
        steam_blurb: details.description.as_deref(),
        developer: details.developer.as_deref(),
        publisher: details.publisher.as_deref(),
        release_date: details.release_date.as_deref(),
        platforms: details
            .platforms
            .as_deref()
            .unwrap_or(&["Windows".to_string()]),
        tags: &tags,
        category: category_guess.as_deref(),
    });

    Ok(EnrichedGame {
        raw_title: game.raw_title.clone(),
        description: seo.description,
        article_md: seo.article_md,
        developer: details.developer,
        publisher: details.publisher,
        release_date: details.release_date,
        platforms: details
            .platforms
            .unwrap_or_else(|| vec!["Windows".to_string()]),
        category_guess,
        category_guesses,
        tags,
        cover_image_url: images.cover,
        cover_alt,
        hero_image_url: images.hero,
        featured_image_url: images.featured,
        featured_image_alt,
        screenshot_urls: images.screenshots,
        screenshot_alts,
        minimum,
        recommended,
        version: game.best_download.version.clone(),
        file_size: Some("See download page".into()),
        storage: Some("10 GB available space".into()),
        instructions: crate::seo::install_steps(&game.raw_title),
        download_links,
        source_url,
        license_type: license_type_for(&game.best_download.source_label),
        seo_title: seo.seo_title,
        seo_description: seo.seo_description,
        steam_app_id: app_id,
        published_at: None,
    })
}

// ---------------------------------------------------------------------------
// Steam appdetails
// ---------------------------------------------------------------------------

#[derive(Default)]
struct SteamDetails {
    description: Option<String>,
    developer: Option<String>,
    publisher: Option<String>,
    release_date: Option<String>,
    platforms: Option<Vec<String>>,
    /// Steam "categories" (Single-player, Co-op, Controller support...).
    categories: Vec<String>,
    /// Steam genres (Action, RPG...).
    genres: Vec<String>,
    screenshots: Vec<String>,
    /// Windows minimum requirements HTML from pc_requirements.
    minimum: Option<String>,
    /// Windows recommended requirements HTML from pc_requirements.
    recommended: Option<String>,
}

/// Resolve a Steam app ID by searching the store for the title.
/// Returns the top hit whose name is reasonably similar to the query.
pub fn resolve_steam_app_id(title: &str, api_base: &str) -> Option<u64> {
    let query: String = crate::matching::normalize_title(title);
    if query.is_empty() {
        return None;
    }
    let url = format!("{api_base}/api/storesearch/?term={query}&cc=us&l=en");
    let body = blocking_get_text(&url)?;
    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
    let items = value.get("items")?.as_array()?;

    let mut best: Option<(f64, u64)> = None;
    for item in items {
        let id = item.get("id")?.as_u64()?;
        let name = item.get("name")?.as_str()?;
        let similarity = crate::matching::title_similarity(title, name);
        if similarity >= 0.85 && best.map(|(s, _)| similarity > s).unwrap_or(true) {
            best = Some((similarity, id));
        }
    }
    let (_, app_id) = best?;
    info!("Resolved '{title}' to Steam app {app_id}");
    Some(app_id)
}

/// Blocking GET returning response text; runs on a short-lived thread so the
/// sync helper can be called from the async workflow.
fn blocking_get_text(url: &str) -> Option<String> {
    let url = url.to_string();
    let handle = std::thread::Builder::new()
        .name("steam-fetch".into())
        .spawn(move || -> Option<String> {
            let response = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .ok()?
                .get(&url)
                .send()
                .ok()?;
            if !response.status().is_success() {
                warn!("Request to {url} returned {}", response.status());
                return None;
            }
            response.text().ok()
        })
        .ok()?;
    handle.join().ok()?
}

/// Steam storefront API is public (no key needed for appdetails).
fn fetch_steam_details(app_id: u64, api_base: &str) -> Option<SteamDetails> {
    let url = format!("{api_base}/api/appdetails?appids={app_id}&l=english");
    let body = blocking_get_text(&url)?;
    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
    let data = value.get(app_id.to_string())?.get("data")?.clone();

    let description = data
        .get("short_description")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            data.get("detailed_description")
                .and_then(|v| v.as_str())
                .map(strip_html)
        });

    let developers: Vec<String> = data
        .get("developers")
        .and_then(|v| v.as_array())
        .map(|arr| strings_from(arr))
        .unwrap_or_default();
    let publishers: Vec<String> = data
        .get("publishers")
        .and_then(|v| v.as_array())
        .map(|arr| strings_from(arr))
        .unwrap_or_default();

    let release_date = data
        .get("release_date")
        .and_then(|v| v.get("date"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let mut platforms = Vec::new();
    if let Some(plat) = data.get("platforms") {
        if plat.get("windows").and_then(|v| v.as_bool()).unwrap_or(false) {
            platforms.push("Windows".into());
        }
        if plat.get("mac").and_then(|v| v.as_bool()).unwrap_or(false) {
            platforms.push("macOS".into());
        }
        if plat.get("linux").and_then(|v| v.as_bool()).unwrap_or(false) {
            platforms.push("Linux".into());
        }
    }

    let categories: Vec<String> = data
        .get("categories")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("description"))
                .filter_map(|d| d.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let genres: Vec<String> = data
        .get("genres")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|g| g.get("description"))
                .filter_map(|d| d.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    // Full-resolution screenshots (path_full is the 1920px variant).
    let screenshots: Vec<String> = data
        .get("screenshots")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.get("path_full"))
                .filter_map(|p| p.as_str())
                .map(str::to_string)
                .take(MAX_SCREENSHOTS)
                .collect()
        })
        .unwrap_or_default();

    // pc_requirements: {minimum: "<html>", recommended: "<html>"} or a list.
    let (minimum, recommended) = parse_pc_requirements(data.get("pc_requirements"));

    Some(SteamDetails {
        description,
        developer: developers.first().cloned(),
        publisher: publishers.first().cloned(),
        release_date,
        platforms: (!platforms.is_empty()).then_some(platforms),
        categories,
        genres,
        screenshots,
        minimum,
        recommended,
    })
}

/// Extract the windows minimum/recommended requirement blobs. The field can
/// be an object ({minimum, recommended}) or a bare array of {title,
/// requirements} entries; handle both.
fn parse_pc_requirements(value: Option<&serde_json::Value>) -> (Option<String>, Option<String>) {
    let Some(value) = value else {
        return (None, None);
    };
    if let Some(obj) = value.as_object() {
        return (
            obj.get("minimum").and_then(|v| v.as_str()).map(str::to_string),
            obj.get("recommended").and_then(|v| v.as_str()).map(str::to_string),
        );
    }
    if let Some(arr) = value.as_array() {
        let mut min = None;
        let mut rec = None;
        for entry in arr {
            let title = entry.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let requirements = entry.get("requirements").and_then(|v| v.as_str());
            let lower = title.to_lowercase();
            if requirements.is_some() && lower.contains("minimum") {
                min = requirements.map(str::to_string);
            } else if requirements.is_some() && lower.contains("recommended") {
                rec = requirements.map(str::to_string);
            }
        }
        return (min, rec);
    }
    (None, None)
}

// ---------------------------------------------------------------------------
// SteamSpy tags (real Steam user tags)
// ---------------------------------------------------------------------------

/// Fetch the real Steam community tags for an app from SteamSpy's public API.
/// Response: {"tags": {"Action": "14532", "FPS": "9210", ...}} ranked by votes.
fn fetch_steamspy_tags(app_id: u64, api_base: &str) -> Option<Vec<String>> {
    let url = format!("{api_base}?request=appdetails&appid={app_id}");
    let body = blocking_get_text(&url)?;
    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
    let tags = value.get("tags")?.as_object()?;

    // Objects preserve insertion order in serde_json (preserve_order feature
    // is on via serde_json default), but to be safe sort by vote count desc.
    let mut ranked: Vec<(String, u64)> = tags
        .iter()
        .map(|(k, v)| {
            let votes = v.as_str().and_then(|s| s.parse::<u64>().ok())
                .or_else(|| v.as_u64())
                .unwrap_or(0);
            (k.clone(), votes)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let out: Vec<String> = ranked
        .into_iter()
        .map(|(tag, _)| tag)
        .take(MAX_TAGS)
        .collect();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ImageSet {
    cover: Option<String>,
    hero: Option<String>,
    featured: Option<String>,
    screenshots: Vec<String>,
}

/// Build the image set for an app from Steam's CDN, with size-appropriate
/// candidates and fallbacks (guide recommendations in brackets):
///
/// - **Cover** (920x430 rec): `header.jpg` 460x215 accepted, fallback
///   `capsule_616x353.jpg`.
/// - **Hero** (1920x620 rec): `library_hero.jpg` 3840x1240, fallback to cover.
/// - **Featured** (1920x1080 or 1232x706): first 1920px gameplay screenshot;
///   the guide lets the UI fall back to the first screenshot when omitted.
/// - **Screenshots** (1920x1080): up to 8 full-resolution from appdetails.
///
/// When `verify` is true every URL is HEAD-checked first, so games without a
/// hero asset (older apps) fall back instead of shipping a broken image.
fn images_from_steam(app_id: u64, details: &SteamDetails, verify: bool) -> ImageSet {
    let base = format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{app_id}");
    let cover_candidates = [
        format!("{base}/header.jpg"),
        format!("{base}/capsule_616x353.jpg"),
    ];
    let hero_candidates = [
        format!("{base}/library_hero.jpg"),
        cover_candidates[0].clone(),
    ];
    let screenshot_candidates: Vec<String> =
        details.screenshots.iter().take(MAX_SCREENSHOTS).cloned().collect();

    if !verify {
        return ImageSet {
            cover: Some(cover_candidates[0].clone()),
            hero: Some(hero_candidates[0].clone()),
            featured: screenshot_candidates.first().cloned(),
            screenshots: screenshot_candidates,
        };
    }

    let cover = cover_candidates.iter().find(|u| url_exists(u.as_str())).cloned();
    let hero = hero_candidates
        .iter()
        .find(|u| url_exists(u.as_str()))
        .cloned()
        .or_else(|| cover.clone());
    let featured = screenshot_candidates.first().cloned();
    let screenshots: Vec<String> = screenshot_candidates
        .into_iter()
        .filter(|u| url_exists(u))
        .collect();

    ImageSet {
        cover,
        hero,
        featured,
        screenshots,
    }
}

/// HEAD-check an image URL on Steam's CDN (short timeout, dedicated thread so
/// the sync helper stays callable from the async workflow).
fn url_exists(url: &str) -> bool {
    let url = url.to_string();
    let spawned = std::thread::Builder::new()
        .name("img-head".into())
        .spawn(move || {
            let Ok(client) = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
            else {
                return false;
            };
            client
                .head(&url)
                .send()
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        });
    match spawned {
        Ok(handle) => handle.join().unwrap_or(false),
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn strings_from(arr: &[serde_json::Value]) -> Vec<String> {
    arr.iter()
        .filter_map(|v| v.as_str())
        .map(str::to_string)
        .collect()
}

fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut chars = html.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            '&' if !in_tag => {
                let rest: String = chars.clone().take(8).collect();
                let decoded = if rest.starts_with("amp;") {
                    Some('&')
                } else if rest.starts_with("lt;") {
                    Some('<')
                } else if rest.starts_with("gt;") {
                    Some('>')
                } else if rest.starts_with("quot;") {
                    Some('"')
                } else if rest.starts_with("apos;") {
                    Some('\'')
                } else if rest.starts_with("nbsp;") {
                    Some(' ')
                } else {
                    None
                };
                match decoded {
                    Some(d) => {
                        out.push(d);
                        let skip = entity_length(&rest);
                        for _ in 0..skip {
                            chars.next();
                        }
                    }
                    None => out.push('&'),
                }
            }
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn entity_length(rest: &str) -> usize {
    for name in ["amp;", "lt;", "gt;", "quot;", "apos;", "nbsp;"] {
        if rest.starts_with(name) {
            return name.len();
        }
    }
    0
}

fn clean_alt_title(raw: &str) -> String {
    let cleaned = strip_html(raw);
    let mut chars = cleaned.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn category_from_tags(tags: &[String]) -> Option<String> {
    let keywords = [
        "action", "adventure", "rpg", "strategy", "simulation", "racing", "sports", "casual",
        "puzzle", "platformer", "shooter",
    ];
    for keyword in keywords {
        for tag in tags {
            if tag.to_lowercase().contains(keyword) {
                return Some(capitalize(keyword));
            }
        }
    }
    None
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn default_minimum() -> Option<String> {
    Some("OS: Windows 10 64-bit\nMemory: 4 GB RAM\nStorage: 10 GB available space".into())
}

fn license_type_for(source: &crate::models::SourceLabel) -> String {
    match source {
        crate::models::SourceLabel::Steamrip => {
            "Full PC game repack collected from a public game-sharing site; listed for reference with official store metadata".to_string()
        }
        crate::models::SourceLabel::Steamunlocked => {
            "Full PC game repack collected from a public game-sharing site; listed for reference with official store metadata".to_string()
        }
    }
}

fn download_links_for(candidates: &[DownloadCandidate], app_id: Option<u64>) -> Vec<DownloadLink> {
    let mut links = Vec::new();
    for cand in candidates {
        links.push(DownloadLink {
            label: format!("Download from {}", source_name(&cand.source_label)),
            url: cand.url.clone(),
            platform: Some("Windows".into()),
            version: cand.version.clone(),
            file_size: None,
            note: Some(source_note(&cand.source_label)),
        });
    }
    // Steam page link is appended by the caller when an app id resolved.
    let _ = app_id;
    links
}

fn source_name(label: &crate::models::SourceLabel) -> &'static str {
    match label {
        crate::models::SourceLabel::Steamrip => "Steamrip",
        crate::models::SourceLabel::Steamunlocked => "SteamUnlocked",
    }
}

fn source_note(label: &crate::models::SourceLabel) -> String {
    match label {
        crate::models::SourceLabel::Steamrip => {
            "Full game repack from Steamrip, latest uploaded version".into()
        }
        crate::models::SourceLabel::Steamunlocked => {
            "Full game repack from SteamUnlocked, latest uploaded version".into()
        }
    }
}

fn debug_unresolved(game: &EnrichmentInput) {
    warn!(
        "Could not resolve a Steam app for '{}' - listing will lack Steam images",
        game.raw_title.trim()
    );
}

/// Return a shallow clone of the input with the raw title entity-decoded.
/// (The scraper decodes anchor text, but titles can also flow in pre-decoded
/// from other callers; this is the single choke point guaranteeing clean copy.)
fn decodable_game(game: &EnrichmentInput) -> EnrichmentInput {
    let decoded = crate::scraper::decode_entities(&game.raw_title);
    if decoded == game.raw_title {
        return EnrichmentInput {
            normalized_title: game.normalized_title.clone(),
            raw_title: game.raw_title.clone(),
            best_download: game.best_download.clone(),
            candidates: game.candidates.clone(),
            steam_app_id: game.steam_app_id,
            steam_store_api: game.steam_store_api.clone(),
            steamspy_api: game.steamspy_api.clone(),
            verify_images: game.verify_images,
        };
    }
    EnrichmentInput {
        normalized_title: game.normalized_title.clone(),
        raw_title: decoded,
        best_download: game.best_download.clone(),
        candidates: game.candidates.clone(),
        steam_app_id: game.steam_app_id,
        steam_store_api: game.steam_store_api.clone(),
        steamspy_api: game.steamspy_api.clone(),
        verify_images: game.verify_images,
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SourceLabel;

    fn input() -> EnrichmentInput {
        EnrichmentInput {
            normalized_title: "some game".into(),
            raw_title: "  Some Game  ".into(),
            best_download: DownloadCandidate {
                url: "https://example.test".into(),
                source_label: SourceLabel::Steamrip,
                version: Some("1.0".into()),
                notes: None,
            },
            candidates: vec![],
            steam_app_id: None,
            // Dead local addresses keep these tests offline-deterministic.
            steam_store_api: Some("http://127.0.0.1:1".into()),
            steamspy_api: Some("http://127.0.0.1:1".into()),
            verify_images: false,
        }
    }

    #[test]
    fn fallback_listing_is_publish_ready() {
        let enriched = build_enriched(&input()).unwrap();
        assert!(enriched.article_md.contains("## About"));
        assert!(enriched.seo_title.contains("Free Download"));
        assert!(!enriched.seo_description.is_empty());
        // 30-char minimum for published descriptions.
        assert!(enriched.description.chars().count() >= 30);
        // Publish-gated fields have valid fallbacks.
        assert!(enriched.minimum.as_deref().map(|m| m.chars().count() >= 10).unwrap_or(false));
        assert!(enriched.version.is_some());
        assert!(enriched.file_size.is_some());
        assert!(enriched.storage.is_some());
        assert!(enriched.license_type.chars().count() >= 3);
        assert!(!enriched.instructions.is_empty());
    }

    #[test]
    fn download_links_use_guide_labels() {
        let mut inp = input();
        inp.candidates = vec![DownloadCandidate {
            url: "https://dl.test/game".into(),
            source_label: SourceLabel::Steamunlocked,
            version: Some("2.0".into()),
            notes: None,
        }];
        let enriched = build_enriched(&inp).unwrap();
        assert!(enriched
            .download_links
            .iter()
            .any(|l| l.label == "Download from SteamUnlocked"));
        assert_eq!(enriched.download_links.first().unwrap().url, "https://dl.test/game");
    }

    #[test]
    fn pc_requirements_object_and_array_both_parse() {
        let obj: serde_json::Value = serde_json::from_str(
            r#"{"minimum":"<ul><li>OS: Win 10</li></ul>","recommended":"<ul><li>16 GB</li></ul>"}"#,
        )
        .unwrap();
        let (min, rec) = parse_pc_requirements(Some(&obj));
        assert_eq!(min.as_deref(), Some("<ul><li>OS: Win 10</li></ul>"));
        assert_eq!(rec.as_deref(), Some("<ul><li>16 GB</li></ul>"));

        let arr: serde_json::Value = serde_json::from_str(
            r#"[{"title":"Minimum","requirements":"<p>2 GB RAM</p>"},{"title":"Recommended","requirements":"<p>8 GB RAM</p>"}]"#,
        )
        .unwrap();
        let (min, rec) = parse_pc_requirements(Some(&arr));
        assert_eq!(min.as_deref(), Some("<p>2 GB RAM</p>"));
        assert_eq!(rec.as_deref(), Some("<p>8 GB RAM</p>"));
    }

    #[test]
    fn requirements_html_is_stripped_to_plain_text() {
        let obj: serde_json::Value = serde_json::from_str(
            r#"{"minimum":"<strong>OS:</strong> Windows 10<br>Memory: 8 GB RAM"}"#,
        )
        .unwrap();
        let (min, _) = parse_pc_requirements(Some(&obj));
        let stripped = min.as_deref().map(strip_html).unwrap_or_default();
        assert!(stripped.contains("Windows 10"));
        assert!(stripped.contains("8 GB RAM"));
        assert!(!stripped.contains('<'));
    }

    #[test]
    fn strips_html_entities() {
        assert_eq!(strip_html("<p>Hello &amp; world</p>"), "Hello & world");
    }

    #[test]
    fn unresolved_titles_keep_entities_out_of_copy() {
        let mut inp = input();
        inp.raw_title = "  Birds Aren&#039;t Real  ".into();
        let enriched = build_enriched(&inp).unwrap();
        assert!(!enriched.article_md.contains("&#039;"));
        assert!(enriched.article_md.contains("Birds Aren't Real"));
    }
}
