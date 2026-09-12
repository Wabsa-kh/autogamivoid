use crate::models::{DownloadCandidate, EnrichedGame};
use tracing::{info, warn};

pub const DEFAULT_STEAM_STORE_API: &str = "https://store.steampowered.com";

pub struct EnrichmentInput {
    pub normalized_title: String,
    pub raw_title: String,
    pub best_download: DownloadCandidate,
    pub candidates: Vec<DownloadCandidate>,
    pub steam_app_id: Option<u64>,
    /// Steam storefront base for appdetails/storesearch (tests point this at a mock).
    pub steam_store_api: Option<String>,
    /// HEAD-verify image URLs against Steam's CDN before publishing so no
    /// listing ships a dead image. Disable in offline tests.
    pub verify_images: bool,
}

/// Enrich a matched game with Steam store data and SteamDB-style image URLs.
///
/// When `steam_app_id` is known the Steam storefront API provides description,
/// developer, publisher, release date, platforms and tags; when it is not, we
/// resolve the title via store search first. If Steam is unreachable entirely,
/// source-derived fallbacks keep the listing complete — except that cover and
/// hero images require an app ID, so games Steam can't confirm are marked as
/// errors by the caller (validate_minimum rejects missing images).
pub fn build_enriched(game: &EnrichmentInput) -> anyhow::Result<EnrichedGame> {
    let api_base = game
        .steam_store_api
        .clone()
        .unwrap_or_else(|| DEFAULT_STEAM_STORE_API.to_string());

    let app_id = match game.steam_app_id {
        Some(id) => Some(id),
        None => resolve_steam_app_id(&game.raw_title, &api_base),
    };

    let (mut details, images) = match app_id {
        Some(app_id) => {
            let details = fetch_steam_details(app_id, &api_base);
            if details.is_none() {
                warn!("Steam details unavailable for app {app_id}; using fallbacks");
            }
            let details = details.unwrap_or_default();
            let images = images_from_steam(app_id, &details, game.verify_images);
            (details, images)
        }
        None => {
            debug_unresolved(game);
            (SteamDetails::default(), ImageSet::default())
        }
    };

    let steam_blurb = details.description.clone();
    let category = best_guess_category(&details.tags);
    let downloads = if game.candidates.is_empty() {
        vec![game.best_download.clone()]
    } else {
        game.candidates.clone()
    };

    let platforms = details
        .platforms
        .take()
        .unwrap_or_else(|| vec!["Windows".into()]);

    let steam_store_url = game
        .steam_app_id
        .map(|id| format!("https://store.steampowered.com/app/{id}"));

    let seo = crate::seo::build_listing(&crate::seo::SeoInput {
        title: &game.raw_title,
        version: game.best_download.version.as_deref(),
        source_label: &game.best_download.source_label,
        steam_blurb: steam_blurb.as_deref(),
        developer: details.developer.as_deref(),
        publisher: details.publisher.as_deref(),
        release_date: details.release_date.as_deref(),
        platforms: &platforms,
        tags: &details.tags,
        category: category.as_deref(),
    });

    Ok(EnrichedGame {
        normalized_title: game.normalized_title.clone(),
        raw_title: game.raw_title.clone(),
        description: seo.description_html,
        developer: details.developer,
        publisher: details.publisher,
        release_date: details.release_date,
        platforms,
        tags: details.tags,
        category,
        cover_image_url: images.cover,
        hero_image_url: images.hero,
        screenshot_urls: images.screenshots,
        downloads,
        steam_store_url,
        meta_title: seo.meta_title,
        meta_description: seo.meta_description,
        meta_keywords: seo.meta_keywords,
        published_at: None,
    })
}

#[derive(Default)]
struct SteamDetails {
    description: Option<String>,
    developer: Option<String>,
    publisher: Option<String>,
    release_date: Option<String>,
    platforms: Option<Vec<String>>,
    tags: Vec<String>,
    screenshots: Vec<String>,
}

#[derive(Default)]
struct ImageSet {
    cover: Option<String>,
    hero: Option<String>,
    screenshots: Vec<String>,
}

/// Resolve a Steam app ID by searching the store for the title.
/// Returns the top hit whose name is reasonably similar to the query.
pub fn resolve_steam_app_id(title: &str, api_base: &str) -> Option<u64> {
    let query: String = crate::matching::normalize_title(title);
    if query.is_empty() {
        return None;
    }
    let url = format!(
        "{api_base}/api/storesearch/?term={query}&cc=us&l=en"
    );
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
                warn!("Steam request to {url} returned {}", response.status());
                return None;
            }
            response.text().ok()
        })
        .ok()?;
    handle.join().ok()?
}

/// Steam storefront API is public (no key needed for appdetails).
/// Runs a short blocking request on a dedicated thread to stay sync-friendly.
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

    // Categories double as broad tags when the tag API is not wired.
    let mut tags: Vec<String> = data
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
    tags.extend(genres);

    // Full-resolution screenshots (path_full is the 1920px variant), capped.
    let screenshots: Vec<String> = data
        .get("screenshots")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.get("path_full"))
                .filter_map(|p| p.as_str())
                .map(str::to_string)
                .take(6)
                .collect()
        })
        .unwrap_or_default();

    Some(SteamDetails {
        description,
        developer: developers.first().cloned(),
        publisher: publishers.first().cloned(),
        release_date,
        platforms: (!platforms.is_empty()).then_some(platforms),
        tags,
        screenshots,
    })
}

/// Build the image set for an app from Steam's CDN, with size-appropriate
/// candidates and fallbacks:
///
/// - **Cover** (2:1 card): `header.jpg` 460x215, fallback `capsule_616x353.jpg`.
/// - **Hero** (wide banner): `library_hero.jpg` 3840x1240, fallback to the cover.
/// - **Screenshots**: up to 6 full-resolution (1920px) images from appdetails.
///
/// When `verify` is true every URL is HEAD-checked against the CDN first, so
/// games without a hero asset (older/smaller apps) fall back instead of
/// publishing a broken image.
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
    let screenshot_candidates: Vec<String> = details.screenshots.iter().take(6).cloned().collect();

    if !verify {
        return ImageSet {
            cover: Some(cover_candidates[0].clone()),
            hero: Some(hero_candidates[0].clone()),
            screenshots: screenshot_candidates,
        };
    }

    let cover = cover_candidates.iter().find(|u| url_exists(u.as_str())).cloned();
    let hero = hero_candidates
        .iter()
        .find(|u| url_exists(u.as_str()))
        .cloned()
        .or_else(|| cover.clone());
    let screenshots: Vec<String> = screenshot_candidates
        .into_iter()
        .filter(|u| url_exists(u))
        .collect();

    ImageSet {
        cover,
        hero,
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
                // Decode the common named entities; unknown ones pass through.
                let rest: String = chars.clone().take(8).collect();
                let decoded = if let Some(after) = rest.strip_prefix("amp;") {
                    let _ = after;
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

fn debug_unresolved(game: &EnrichmentInput) {
    warn!(
        "Could not resolve a Steam app for '{}' - listing will lack Steam images",
        game.raw_title.trim()
    );
}

fn best_guess_category(tags: &[String]) -> Option<String> {
    let keywords = [
        "action", "adventure", "rpg", "strategy", "simulation", "racing", "shooter", "sports",
        "survival", "puzzle", "platformer", "sandbox",
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
            // Dead local address keeps these tests offline-deterministic.
            steam_store_api: Some("http://127.0.0.1:1".into()),
            // Offline: no HEAD verification against the real CDN.
            verify_images: false,
        }
    }

    #[test]
    fn seo_description_mentions_version_and_meta_fields() {
        let enriched = build_enriched(&input()).unwrap();
        assert!(enriched.description.contains("version 1.0"));
        assert!(enriched.description.contains("<h2>"));
        assert!(enriched.meta_title.contains("Free Download"));
        assert!(!enriched.meta_description.trim().is_empty());
        assert!(!enriched.meta_keywords.is_empty());
    }

    #[test]
    fn enriches_platforms_even_without_steam_app_id() {
        let enriched = build_enriched(&input()).unwrap();
        assert!(!enriched.platforms.is_empty());
        assert!(enriched.steam_store_url.is_none());
    }

    #[test]
    fn strips_html_entities() {
        assert_eq!(strip_html("<p>Hello &amp; world</p>"), "Hello & world");
    }
}
