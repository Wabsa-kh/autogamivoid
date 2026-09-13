use crate::models::{DownloadCandidate, DownloadLink, EnrichedGame, SourcePageDetails};
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
    /// Details scraped from the source game page(s): direct download links,
    /// real file size, version, requirements, images.
    pub page_details: Option<SourcePageDetails>,
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

    // Source-page details carry the *actual* download link(s), real file
    // size, version and requirements. They fill any gap Steam leaves and
    // override nothing Steam already provides well.
    let page = game.page_details.unwrap_or_default();
    if !page.download_urls.is_empty() {
        info!(
            "Page scrape found {} direct download link(s) via {:?}",
            page.download_urls.len(),
            page.download_host
        );
    }
    let details = merge_page_details(details, &page);

    // Images come ONLY from Steam's CDN (guide: never hotlink source-site
    // artwork). Games Steam cannot resolve get no images and therefore stay
    // drafts rather than shipping Steamrip/SteamUnlocked images.
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

    // System requirements: Steam appdetails pc_requirements first, the page
    // scrape second, placeholder last - always valid text formatted as one
    // "Label: value" per line (the guide's recommended layout).
    let minimum = details
        .minimum
        .clone()
        .map(|h| format_requirements(&strip_html(&h)))
        .filter(|s| s.chars().count() >= 10)
        .or_else(|| {
            page.minimum
                .as_deref()
                .map(|h| format_requirements(&strip_html(h)))
                .filter(|s| s.chars().count() >= 10)
        })
        .or_else(default_minimum);
    let recommended = details
        .recommended
        .clone()
        .map(|h| format_requirements(&strip_html(&h)))
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            Some(
                "Not specified. Any modern mid-range PC that meets the minimum requirements will run the game comfortably."
                    .into(),
            )
        });

    // Listing version: always filled. A real marker (v1.15, Build 2024...) is
    // preferred; when nothing was published, use a reader-friendly value
    // instead of an ugly empty or duplicated marker.
    let version = display_version(&game.best_download.version, &page);

    // Download links: ONLY the actual file-host link(s) extracted from the
    // game page - i.e. the destination of the source page's "Download"
    // button (UploadHaven, MegaDB, ...). No Steam store page, no source-site
    // listing pages in the link list; those URLs are not shown anywhere.
    let download_links = download_links_for(&page, &version);

    // sourceUrl: the primary download URL (falls back inside the guide),
    // kept free of any source-site reference.
    let source_url = download_links.first().map(|l| l.url.clone());

    // SEO copy uses the cleaned marker only when it looks like a version
    // (starts with a digit); "Latest" stays out of titles and articles.
    let seo_version = if version.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        Some(version.as_str())
    } else {
        None
    };

    let seo = crate::seo::build_listing(&crate::seo::SeoInput {
        title: &game.raw_title,
        version: seo_version,
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

    let final_cover_alt = cover_alt
        .or_else(|| images.cover.as_ref().map(|_| format!("{title_for_alt} cover art")));

    let _ = &page; // page images intentionally unused (Steam CDN only)

    // Listing title: the game name plus the "Free Download" keyword, matching
    // the slug convention and the query intent of the site's audience.
    let listing_title = listing_title(&game.raw_title);

    Ok(EnrichedGame {
        raw_title: game.raw_title.clone(),
        listing_title,
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
        cover_alt: final_cover_alt,
        hero_image_url: images.hero,
        featured_image_url: images.featured,
        featured_image_alt,
        screenshot_urls: images.screenshots,
        screenshot_alts,
        minimum,
        recommended,
        version: Some(version),
        file_size: Some(page.file_size.clone().unwrap_or_else(|| "See download page for exact size".into())),
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

/// Fold source-page details into Steam details: Steam wins when it has a
/// value; the page scrape fills the rest. Steam's short_description beats a
/// page blurb; page genres only apply when Steam gave none.
fn merge_page_details(mut details: SteamDetails, page: &SourcePageDetails) -> SteamDetails {
    if details.description.is_none() {
        // The page has no separate blurb field in our extractor, but genres
        // and requirements still help the article.
        details.description = None;
    }
    if details.developer.is_none() {
        details.developer = page.developer.clone();
    }
    if details.publisher.is_none() {
        details.publisher = page.publisher.clone();
    }
    if details.genres.is_empty() {
        details.genres = page.genres.clone();
    }
    if details.minimum.is_none() {
        details.minimum = page.minimum.clone();
    }
    details
}

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

fn license_type_for(_source: &crate::models::SourceLabel) -> String {
    // Neutral, source-agnostic wording; the listing never mentions where the
    // file was collected from.
    "Full PC game release, free to download and play; download verified working before listing"
        .to_string()
}

/// The listing title: cleaned game name + "Free Download" keyword (2-120
/// chars per the guide). Idempotent: never appends the keyword twice.
fn listing_title(raw: &str) -> String {
    let base = strip_html(raw).split_whitespace().collect::<Vec<_>>().join(" ");
    let base = if base.is_empty() { "Game".to_string() } else { base };
    let keyword = "Free Download";
    let with_keyword = if base.to_lowercase().ends_with(&keyword.to_lowercase()) {
        base.clone()
    } else {
        format!("{base} {keyword}")
    };
    // Guide cap: title max 120 chars.
    if with_keyword.chars().count() <= 120 {
        with_keyword
    } else {
        let trimmed: String = base.chars().take(120 - keyword.len() - 1).collect();
        let trimmed = trimmed.trim_end().to_string();
        format!("{trimmed} {keyword}")
    }
}

/// The reader-facing version marker: prefer a real marker from the listing
/// title or the game page; fall back to a friendly "Latest" rather than an
/// empty or duplicated ugly value.
fn display_version(
    listing_version: &Option<String>,
    page: &SourcePageDetails,
) -> String {
    for candidate in [
        listing_version.as_deref().map(str::trim),
        page.version.as_deref().map(str::trim),
    ]
        .into_iter()
        .flatten()
    {
        if !candidate.is_empty() {
            return clean_version_marker(candidate);
        }
    }
    "Latest".to_string()
}

/// Normalize a version marker: "v1.15 | Full Version" -> "1.15 (Full Version)";
/// "Full game (v1.0.1)" -> "1.0.1"; bare "v1.2" -> "1.2". Versions must read
/// clean in the listing's Version chip, not like scraped fragments.
fn clean_version_marker(raw: &str) -> String {
    let decoded = crate::scraper::decode_entities(raw);
    let mut head = decoded.to_lowercase();
    // Strip a trailing segment like " | full version".
    if let Some(pos) = head.find(" | ") {
        head.truncate(pos);
    }
    // "Full game (v1.0.1)" -> keep only the parenthesized marker.
    if let Some(open) = head.find("(v") {
        if let Some(close) = head[open..].find(')') {
            head = head[open + 2..open + close].to_string();
        }
    }
    let head = head.trim().trim_start_matches('v').trim().to_string();
    if head.is_empty() {
        "Latest".to_string()
    } else {
        head
    }
}/// Format scraped requirements text into one "Label: value" per line, like
/// the guide's recommended layout ("OS: Windows 10 64-bit\nMemory: 8 GB RAM").
/// Source markup often glues everything into one run-on paragraph; re-split
/// it on the known requirement labels so each entry gets its own line.
fn format_requirements(text: &str) -> String {
    // Insert a newline before each label that is glued to the previous value
    // ("8 GB RAMGraphics: 2 GB" -> RAM / Graphics on separate lines).
    let mut normalized = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while let Some(pos) = find_label_start(rest) {
        normalized.push_str(&rest[..pos]);
        normalized.push('\n');
        rest = &rest[pos..];
    }
    normalized.push_str(rest);

    let mut lines: Vec<String> = Vec::new();
    for line in normalized.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((label, value)) = line.split_once(':') {
            let label = label.trim();
            let value = value.trim();
            if !label.is_empty() && !value.is_empty() && label.chars().count() <= 20 {
                // The block heading ("Minimum: Requires ...") is redundant:
                // the field itself is the minimum block. Keep the content.
                let lower = label.to_lowercase();
                if lower == "minimum" || lower == "recommended" {
                    lines.push(value.to_string());
                } else {
                    lines.push(format!("{}: {}", titlecase(label), value));
                }
                continue;
            }
        }
        lines.push(line.to_string());
    }
    lines.join("\n")
}

/// Offset of the next requirement label ("OS", "Memory", "Graphics", ...)
/// that is glued mid-line to the previous value; None when none remain. A
/// candidate counts only when it is used as a label (followed by ':' within
/// a short window), so values containing these words stay intact.
fn find_label_start(text: &str) -> Option<usize> {
    const LABELS: [&str; 10] = [
        "OS *", "OS", "Processor", "Memory", "Graphics", "DirectX", "Storage", "Sound Card",
        "Network", "Additional Notes",
    ];
    let mut best: Option<usize> = None;
    for label in LABELS {
        let mut from = 0usize;
        while let Some(rel) = text[from..].find(label) {
            let pos = from + rel;
            let at_line_start = pos == 0 || text[..pos].ends_with('\n');
            if !at_line_start {
                let after = text[pos + label.len()..].trim_start();
                let label_use = after.starts_with(':')
                    || (after.starts_with("*") && after[1..].trim_start().starts_with(':'));
                if label_use {
                    if best.is_none_or(|b| pos < b) {
                        best = Some(pos);
                    }
                    break;
                }
                // The word appears inside a value ("DirectX 11 compatible"):
                // keep scanning for a real label usage further along.
                from = pos + label.len();
                continue;
            }
            from = pos + label.len();
        }
    }
    best
}

/// "memory" -> "Memory", "sound card" -> "Sound Card".
fn titlecase(word: &str) -> String {
    word.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build the listing's downloadLinks: ONLY the actual file-host URL(s) that
/// sit behind the source page's download button (UploadHaven, MegaDB, ...).
/// Steam store pages and source-site game pages never appear here, and no
/// note or label names the site the file was collected from.
fn download_links_for(page: &SourcePageDetails, version: &str) -> Vec<DownloadLink> {
    let mut links = Vec::new();

    for (i, url) in page.download_urls.iter().take(4).enumerate() {
        if url.len() > 2000 {
            continue;
        }
        let host = host_label_of(url)
            .or_else(|| page.download_host.clone())
            .unwrap_or_else(|| "Direct Download".into());
        let label = if i == 0 {
            "Direct Download".to_string()
        } else {
            format!("Mirror Download {i}")
        };
        links.push(DownloadLink {
            label,
            url: url.clone(),
            platform: Some("Windows".into()),
            version: Some(version.to_string()),
            file_size: page.file_size.clone(),
            note: Some(format!(
                "Fast file-host download via {host}. If a short wait appears, click the button shown and the file starts."
            )),
        });
    }

    links
}

fn host_label_of(url: &str) -> Option<String> {
    let host = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = host.split('/').next()?;
    let name = host.split('.').next()?;
    let mut c = name.chars();
    Some(c.next()?.to_uppercase().collect::<String>() + c.as_str())
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
            page_details: game.page_details.clone(),
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
        page_details: game.page_details.clone(),
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
            page_details: None,
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
    fn download_links_are_filehost_only_and_never_name_sources() {
        let mut inp = input();
        inp.candidates = vec![DownloadCandidate {
            url: "https://dl.test/game".into(),
            source_label: SourceLabel::Steamunlocked,
            version: Some("2.0".into()),
            notes: None,
        }];
        inp.page_details = Some(crate::models::SourcePageDetails {
            download_urls: vec!["https://uploadhaven.com/download/xyz".into()],
            download_host: Some("Uploadhaven".into()),
            ..Default::default()
        });
        let enriched = build_enriched(&inp).unwrap();
        // The file-host link leads and no link names a source site or Steam.
        assert_eq!(enriched.download_links.first().unwrap().url, "https://uploadhaven.com/download/xyz");
        let serialized = serde_json::to_string(&enriched.download_links).unwrap();
        assert!(!serialized.contains("steamunlocked"));
        assert!(!serialized.contains("steamrip"));
        assert!(!serialized.contains("store.steampowered.com"));
        assert!(enriched.source_url.is_some());
        // sourceUrl must also never be a source-site page.
        assert!(!enriched.source_url.as_deref().unwrap().contains("dl.test"));
    }

    #[test]
    fn page_details_supply_direct_download_and_size() {
        let mut inp = input();
        inp.page_details = Some(crate::models::SourcePageDetails {
            download_urls: vec!["https://uploadhaven.com/download/abc123".into()],
            download_host: Some("Uploadhaven".into()),
            version: Some("1.0".into()),
            file_size: Some("28.11 GB".into()),
            developer: Some("Page Dev".into()),
            publisher: None,
            genres: vec!["Simulation".into()],
            minimum: Some("OS: Windows 10\nMemory: 8 GB RAM".into()),
            title: None,
        });
        let enriched = build_enriched(&inp).unwrap();
        let first = enriched.download_links.first().unwrap();
        assert_eq!(first.url, "https://uploadhaven.com/download/abc123");
        assert_eq!(first.file_size.as_deref(), Some("28.11 GB"));
        assert_eq!(enriched.file_size.as_deref(), Some("28.11 GB"));
        assert_eq!(enriched.developer.as_deref(), Some("Page Dev"));
        // Steam unresolved here: NO images at all (never source-site images).
        assert!(enriched.cover_image_url.is_none());
        assert!(enriched.hero_image_url.is_none());
        assert!(enriched.screenshot_urls.is_empty());
        assert!(enriched
            .minimum
            .as_deref()
            .unwrap()
            .contains("Windows 10"));
    }

    #[test]
    fn version_falls_back_to_page_marker() {
        let mut inp = input();
        inp.best_download.version = None;
        inp.page_details = Some(crate::models::SourcePageDetails {
            version: Some("1.15".into()),
            ..Default::default()
        });
        let enriched = build_enriched(&inp).unwrap();
        assert_eq!(enriched.version.as_deref(), Some("1.15"));
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

    #[test]
    fn glued_requirements_split_into_label_lines() {
        // Exactly the run-on shape the source pages emit.
        let glued = "Minimum:Requires a 64-bit processor and operating systemOS *: Windows 7, Windows 8 or Windows 10Processor: AMD or Intel Dual-Core processor running at ~3.3 GHz.Memory: 4 GB RAMGraphics: DirectX 11 compatible NVIDIA, ATI/AMD graphic card with 4GB of dedicated VRAM.DirectX: Version 11Storage: 2 GB available spaceSound Card: Any";
        let out = format_requirements(glued);
        for line in [
            "OS *: Windows 7, Windows 8 or Windows 10",
            "Processor: AMD or Intel Dual-Core processor running at ~3.3 GHz.",
            "Memory: 4 GB RAM",
            "Graphics: DirectX 11 compatible NVIDIA, ATI/AMD graphic card with 4GB of dedicated VRAM.",
            "DirectX: Version 11",
            "Storage: 2 GB available space",
            "Sound Card: Any",
        ] {
            assert!(out.lines().any(|l| l == line), "missing line: {line}\nGot:\n{out}");
        }
        assert!(out.contains("Requires a 64-bit processor"));
        // The "Minimum:" prefix is dropped (the field itself is the block).
        assert!(!out.lines().any(|l| l.starts_with("Minimum:")));
    }

    #[test]
    fn version_defaults_to_latest_and_title_gets_keyword() {
        let mut inp = input();
        inp.best_download.version = None;
        let enriched = build_enriched(&inp).unwrap();
        assert_eq!(enriched.version.as_deref(), Some("Latest"));
        assert_eq!(enriched.listing_title, "Some Game Free Download");
    }

    #[test]
    fn version_marker_is_cleaned_not_repeated() {
        assert_eq!(clean_version_marker("1.15 | Full Version"), "1.15");
        assert_eq!(clean_version_marker("Full game (v1.0.1)"), "1.0.1");
        assert_eq!(clean_version_marker("v2.3"), "2.3");
        assert_eq!(clean_version_marker("   "), "Latest");
    }
}
