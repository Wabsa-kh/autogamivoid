use crate::config::SourceEndpoint;
use crate::models::{SourceGame, SourceLabel};
use reqwest::Client;
use std::time::Duration;
use tracing::{info, warn};

/// A-Z hub paths discovered on both sites (Steamunlocked redirects
/// steamunlocked.net -> steamunlocked.org; the config base_url just needs to
/// point at either host and redirects are followed).
pub const STEAMRIP_ALL_GAMES_PATH: &str = "/games-list-page/";
pub const STEAMUNLOCKED_ALL_GAMES_PATH: &str = "/all-games/";

/// Letter pages on Steamunlocked's A-Z hub, including digits.
pub const LETTER_PATHS: [&str; 27] = [
    "0-9", "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q",
    "r", "s", "t", "u", "v", "w", "x", "y", "z",
];

/// Hard bound on pages followed per letter so a markup change can't loop.
const MAX_PAGES_PER_LETTER: usize = 60;

// ---------------------------------------------------------------------------
// Freshness scrape (homepage / listing page) — used by the update-check run.
// ---------------------------------------------------------------------------

/// Scrape the Steamrip listing: enumerate game pages, take each page's main
/// download button URL, and parse the `[vX.Y]` version marker when present.
pub async fn scrape_steamrip(cfg: &SourceEndpoint) -> anyhow::Result<Vec<SourceGame>> {
    scrape_listing(cfg, SourceLabel::Steamrip).await
}

/// Scrape the Steamunlocked listing the same way.
pub async fn scrape_steamunlocked(cfg: &SourceEndpoint) -> anyhow::Result<Vec<SourceGame>> {
    scrape_listing(cfg, SourceLabel::Steamunlocked).await
}

async fn scrape_listing(cfg: &SourceEndpoint, label: SourceLabel) -> anyhow::Result<Vec<SourceGame>> {
    if cfg.base_url.is_empty() {
        warn!("{} base_url is not configured; returning empty list", label);
        return Ok(vec![]);
    }
    info!("Scraping {} listing from: {}", label, cfg.base_url);
    let client = http_client()?;
    let html = fetch_text(&client, cfg, &cfg.base_url).await?;
    let links = extract_game_links(&html, &cfg.base_url);
    info!("{} listing: {} game links", label, links.len());
    Ok(to_source_games(links, label))
}

// ---------------------------------------------------------------------------
// Catalog scrape (A-Z hubs) — used by the bulk catalog-import run.
// ---------------------------------------------------------------------------

/// Walk Steamrip's full A-Z games list (`/games-list-page/`, one giant page).
pub async fn scrape_catalog_steamrip(cfg: &SourceEndpoint) -> anyhow::Result<Vec<SourceGame>> {
    if cfg.base_url.is_empty() {
        warn!("Steamrip base_url is not configured; returning empty list");
        return Ok(vec![]);
    }
    let client = http_client()?;
    let url = join_url(&cfg.base_url, STEAMRIP_ALL_GAMES_PATH);
    info!("Fetching Steamrip A-Z catalog: {url}");
    let html = fetch_text(&client, cfg, &url).await?;
    let links = extract_game_links(&html, &cfg.base_url);
    info!("Steamrip A-Z catalog: {} game links", links.len());
    Ok(to_source_games(links, SourceLabel::Steamrip))
}

/// Walk Steamunlocked's A-Z hub (`/all-games/`), fetching every letter page.
pub async fn scrape_catalog_steamunlocked(cfg: &SourceEndpoint) -> anyhow::Result<Vec<SourceGame>> {
    if cfg.base_url.is_empty() {
        warn!("Steamunlocked base_url is not configured; returning empty list");
        return Ok(vec![]);
    }
    let client = http_client()?;
    let hub_url = join_url(&cfg.base_url, STEAMUNLOCKED_ALL_GAMES_PATH);
    info!("Fetching Steamunlocked A-Z hub: {hub_url}");

    // Discover letter paths from the hub; fall back to the known set.
    let hub_html = fetch_text(&client, cfg, &hub_url).await?;
    let mut letters = extract_letter_paths(&hub_html);
    if letters.is_empty() {
        letters = LETTER_PATHS.iter().map(|s| s.to_string()).collect();
    }
    info!("Steamunlocked letter pages: {}", letters.len());

    let mut all: Vec<SourceGame> = Vec::new();
    for letter in &letters {
        let letter_url = join_url(&hub_url, &format!("{letter}/"));
        let html = match fetch_text(&client, cfg, &letter_url).await {
            Ok(html) => html,
            Err(e) => {
                warn!("Letter page {letter} failed, skipping: {e:#}");
                continue;
            }
        };
        let mut links = extract_game_links(&html, &cfg.base_url);
        let mut page_count = 1usize;

        // Follow letter pagination (/all-games/a/page/2/...) when present.
        let mut next_pages = pagination_paths(&html, &format!("/all-games/{letter}/"));
        while let Some(path) = next_pages.pop() {
            if page_count >= MAX_PAGES_PER_LETTER {
                break;
            }
            let page_url = join_url(&cfg.base_url, &path);
            match fetch_text(&client, cfg, &page_url).await {
                Ok(page_html) => {
                    links.extend(extract_game_links(&page_html, &cfg.base_url));
                    next_pages.extend(pagination_paths(&page_html, &path));
                    page_count += 1;
                }
                Err(e) => {
                    warn!("Page {path} failed, skipping: {e:#}");
                    break;
                }
            }
        }

        info!("Letter {letter}: {} games ({} pages)", links.len(), page_count);
        all.extend(to_source_games(links, SourceLabel::Steamunlocked));
    }

    info!("Steamunlocked A-Z catalog: {} game links", all.len());
    Ok(all)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn http_client() -> anyhow::Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36")
        .build()?)
}

async fn fetch_text(
    client: &Client,
    cfg: &SourceEndpoint,
    url: &str,
) -> anyhow::Result<String> {
    let mut request = client.get(url);
    if let Some(headers) = &cfg.headers {
        for (key, value) in headers {
            request = request.header(key, value);
        }
    }
    let response = request.send().await?;
    if !response.status().is_success() {
        anyhow::bail!("fetch {url} failed: HTTP {}", response.status());
    }
    Ok(response.text().await?)
}

/// Join a possibly-relative path onto a base URL.
fn join_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    if let Some(rest) = path.strip_prefix('/') {
        // The hub URL may itself be under a subpath (/all-games/): letters are
        // relative to the hub.
        if path.len() > 1 && !base.ends_with(STEAMUNLOCKED_ALL_GAMES_PATH.trim_end_matches('/')) {
            // Distinguish absolute-site paths from hub-relative ones at call
            // sites; here treat leading-slash as site-relative.
            return format!("{base}/{rest}");
        }
        return format!("{base}/{rest}");
    }
    format!("{base}/{path}")
}

pub struct GameLink {
    pub title: String,
    pub url: String,
    pub version: Option<String>,
}

/// Yield `(href, inner_html)` for every `<a href="...">...</a>` in the page.
fn iter_anchor_tags(html: &str) -> Vec<(&str, &str)> {
    let mut result = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<a ") {
        let after_open = &rest[start..];
        let end = match after_open.find("</a>") {
            Some(e) => e,
            None => break,
        };
        let tag = &after_open[..end + 4];
        let href = tag
            .find("href=\"")
            .and_then(|h| tag[h + 6..].find('"').map(|e| &tag[h + 6..h + 6 + e]));
        let text = tag
            .find('>')
            .map(|t| &tag[t + 1..tag.len() - 4])
            .unwrap_or("");
        if let Some(href) = href {
            result.push((href, text));
        }
        rest = &rest[start + end + 4..];
    }
    result
}

/// Pull plausible game-page links out of a listing page's HTML.
/// Keeps `<a href>` targets that look like game posts and excludes
/// nav/tag/category/pagination URLs. Relative URLs are joined to the base.
fn extract_game_links(html: &str, base_url: &str) -> Vec<GameLink> {
    let mut links = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for (href, text) in iter_anchor_tags(html) {
        let href = href.trim().to_string();
        let absolute = if href.starts_with("http://") || href.starts_with("https://") {
            href.clone()
        } else if href.starts_with('/') {
            join_url(base_url, &href)
        } else {
            continue;
        };
        if !is_plausible_game_url(&absolute) {
            continue;
        }
        let raw_text = anchor_text(text);
        if raw_text.len() < 2 {
            continue;
        }
        let (title, version) = clean_listing_title(&raw_text);
        if title.len() < 2 {
            continue;
        }
        if seen.insert(absolute.clone()) {
            links.push(GameLink {
                title,
                url: absolute,
                version,
            });
        }
    }
    links
}

/// Discover letter paths (`/all-games/<letter>/`) from the hub page.
fn extract_letter_paths(hub_html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (href, _) in iter_anchor_tags(hub_html) {
        let Some(path) = href_segment(href, STEAMUNLOCKED_ALL_GAMES_PATH) else {
            continue;
        };
        if seen.insert(path.clone()) {
            out.push(path);
        }
    }
    out
}

/// If `href` points at `<prefix><segment>/`, return the segment.
fn href_segment(href: &str, prefix: &str) -> Option<String> {
    let marker = prefix.to_string();
    let idx = href.find(&marker)?;
    let after = &href[idx + marker.len()..];
    let after = after.trim_start_matches('/');
    let segment = after.split('/').next()?;
    if segment.is_empty() || segment.contains("page") || segment.contains('?') {
        return None;
    }
    Some(segment.to_string())
}

/// Find deeper pagination paths like `<current>/page/2/` on a listing page.
fn pagination_paths(html: &str, current_path: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (href, _) in iter_anchor_tags(html) {
        let needle = format!("{current_path}page/");
        if href.contains(&needle) {
            if let Some(page) = href_segment(href, &needle) {
                if page.chars().all(|c| c.is_ascii_digit()) {
                    out.push(format!("{current_path}page/{page}/"));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn is_plausible_game_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    let excluded = [
        "/category/", "/tag/", "/page/", "/wp-", "/author/", "/about", "/contact", "/privacy",
        "/dmca", "/faq", "/terms", "/sitemap", "/collections", "/games-list", "/top-games",
        "/updated-games", "/all-games/", "/recently-updated", "/most-downloaded", "/trending",
        "/latest-news", "/request", "/my-library", "/launcher", "/steps-for-games", "/categories",
        "facebook.com", "twitter.com", "t.me", "discord", "telegram", "#", "mailto:", ".css",
        ".js", ".png", ".jpg", "?p=", "?random-post",
    ];
    !excluded.iter().any(|x| lower.contains(x))
}

fn anchor_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_tag = false;
    for c in raw.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn to_source_games(links: Vec<GameLink>, label: SourceLabel) -> Vec<SourceGame> {
    links
        .into_iter()
        .map(|link| SourceGame {
            title: link.title,
            download_url: link.url,
            version: link.version,
            notes: None,
            source_label: label.clone(),
        })
        .collect()
}

/// Turn a listing anchor like `1 Trait Escape Free Download (v1.15)` into a
/// clean title plus a version marker. Handles `[v1.5]`, `(v1.5)`,
/// `(Build 123456)`, `(v1.6.1 + Multiplayer)` and `Version 4` styles.
pub fn clean_listing_title(raw: &str) -> (String, Option<String>) {
    let mut text = raw.trim().to_string();

    // 1. Version markers in brackets/parens (may sit after "Free Download").
    let mut version = version_from_title(&text);

    // Remove the bracket/paren group once extracted so it doesn't pollute the title.
    text = remove_group(&text, '[', ']').unwrap_or(text);
    text = remove_group(&text, '(', ')').unwrap_or(text);
    text = text.trim().to_string();

    // 2. Marketing suffixes, most specific first.
    let suffixes = [
        " free download",
        " full version",
        " full game",
        " download",
        " pc game",
        " pc",
        " full",
    ];
    let lowered = text.to_lowercase();
    for suffix in suffixes {
        if lowered.ends_with(suffix) {
            text.truncate(text.len() - suffix.len());
            break;
        }
    }

    if version.is_none() {
        version = version_from_title(&text);
    }

    let title = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (title, version)
}

/// Remove the last balanced `open..close` group from a string.
fn remove_group(s: &str, open: char, close: char) -> Option<String> {
    let open_idx = s.rfind(open)?;
    let close_idx = s[open_idx..].find(close)? + open_idx;
    let inner = s[open_idx + open.len_utf8()..close_idx].trim();
    if inner.is_empty() {
        return None;
    }
    Some(format!("{}{}", &s[..open_idx], &s[close_idx + close.len_utf8()..]))
}

/// Parse the downloadable-file version marker from a Steamrip-style title
/// like "Elden Ring [v1.10]" or "Game Name (Build 12345)".
pub fn version_from_title(title: &str) -> Option<String> {
    let t = title.trim();
    // Bracket/paren groups: last group wins, content after '+' ignored.
    for (open, close) in [('[', ']'), ('(', ')')] {
        if let (Some(o), Some(c)) = (t.rfind(open), t.rfind(close)) {
            if c > o {
                let inner = t[o + 1..c].trim();
                let inner = inner.split('+').next().unwrap_or(inner).trim();
                if looks_like_version(inner) {
                    return Some(normalize_version(inner));
                }
            }
        }
    }
    // Trailing "v1.2.3" or "Version 1.2"
    let tokens: Vec<&str> = t.split_whitespace().collect();
    for window in tokens.windows(2).rev() {
        if window[0].eq_ignore_ascii_case("version") {
            let candidate = window[1].trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.');
            if looks_like_version(candidate) {
                return Some(normalize_version(candidate));
            }
        }
    }
    for token in tokens.iter().rev().take(3) {
        let lowered = token.to_lowercase().trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.').to_string();
        if lowered.starts_with('v')
            && lowered
                .chars()
                .nth(1)
                .is_some_and(|c| c.is_ascii_digit())
        {
            let digits: String = lowered.chars().skip(1).collect();
            if !digits.is_empty() {
                return Some(digits);
            }
        }
    }
    None
}

fn normalize_version(s: &str) -> String {
    let lowered = s.trim().to_lowercase();
    let without_build = lowered.strip_prefix("build ").unwrap_or(&lowered);
    without_build.trim_start_matches('v').to_string()
}

fn looks_like_version(s: &str) -> bool {
    let s = s.trim().to_lowercase();
    let s = s.strip_prefix("build ").map(str::trim).unwrap_or(&s);
    let s = s.trim_start_matches('v');
    !s.is_empty() && s.chars().next().is_some_and(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_from_brackets_parens_and_suffixes() {
        assert_eq!(version_from_title("Elden Ring [v1.10]"), Some("1.10".into()));
        assert_eq!(version_from_title("Game Name (v2.3)"), Some("2.3".into()));
        assert_eq!(version_from_title("Game Name (Build 13623225)"), Some("13623225".into()));
        assert_eq!(version_from_title("Game Name (v1.6.1 + Multiplayer)"), Some("1.6.1".into()));
        assert_eq!(version_from_title("Game Name Version 4"), Some("4".into()));
        assert_eq!(version_from_title("Game Name v2.3"), Some("2.3".into()));
        assert_eq!(version_from_title("Plain Game"), None);
    }

    #[test]
    fn cleans_listing_titles() {
        let (title, version) = clean_listing_title("1 Trait Escape Free Download (v1.15)");
        assert_eq!(title, "1 Trait Escape");
        assert_eq!(version.as_deref(), Some("1.15"));

        let (title, version) = clean_listing_title("A Plague Tale: Requiem Free Download (v1.4.0.0)");
        assert_eq!(title, "A Plague Tale: Requiem");
        assert_eq!(version.as_deref(), Some("1.4.0.0"));

        let (title, version) = clean_listing_title("100% Orange Juice Free Download (Build 13623225)");
        assert_eq!(title, "100% Orange Juice");
        assert_eq!(version.as_deref(), Some("13623225"));

        let (title, version) = clean_listing_title("Half-Life 2 Free Download");
        assert_eq!(title, "Half-Life 2");
        assert_eq!(version, None);
    }

    #[test]
    fn extracts_links_and_skips_nav() {
        let html = r#"
            <html><body>
            <a href="/category/action/">Action</a>
            <a href="/page/2/">2</a>
            <a href="https://site.test/game-one-download/">Game One</a>
            <a href="/game-two-free-download/">Game Two</a>
            <a href="https://other.test/game-three/">Game Three</a>
            </body></html>
        "#;
        let links = extract_game_links(html, "https://site.test");
        let urls: Vec<&str> = links.iter().map(|l| l.url.as_str()).collect();
        assert!(urls.contains(&"https://site.test/game-one-download/"));
        assert!(urls.contains(&"https://site.test/game-two-free-download/"));
        assert!(urls.contains(&"https://other.test/game-three/"));
        assert!(!urls.iter().any(|u| u.contains("/category/")));
        assert!(!urls.iter().any(|u| u.contains("/page/")));
    }

    #[test]
    fn extracts_letter_paths_from_hub() {
        let html = r#"
            <a class="su-az-letter" href="/all-games/">All</a>
            <a class="su-az-letter" href="/all-games/0-9/">0-9</a>
            <a class="su-az-letter" href="/all-games/a/">A</a>
            <a class="su-az-letter" href="/all-games/b/">B</a>
            <a href="https://site.test/category/action/">Action</a>
        "#;
        let letters = extract_letter_paths(html);
        assert!(letters.contains(&"0-9".to_string()));
        assert!(letters.contains(&"a".to_string()));
        assert!(letters.contains(&"b".to_string()));
        assert_eq!(letters.len(), 3);
    }

    #[test]
    fn finds_pagination_paths() {
        let html = r#"
            <a href="/all-games/s/page/2/">2</a>
            <a href="/all-games/s/page/3/">3</a>
            <a href="/all-games/s/page/2/">2 again</a>
        "#;
        let pages = pagination_paths(html, "/all-games/s/");
        assert_eq!(pages, vec!["/all-games/s/page/2/", "/all-games/s/page/3/"]);
    }

    #[test]
    fn anchor_text_strips_nested_tags() {
        assert_eq!(anchor_text("<b>Half</b>-Life 2"), "Half-Life 2");
    }

    #[test]
    fn join_url_handles_relative_paths() {
        assert_eq!(
            join_url("https://steamrip.com", "/game-x/"),
            "https://steamrip.com/game-x/"
        );
        assert_eq!(
            join_url("https://steamunlocked.org/all-games", "a/"),
            "https://steamunlocked.org/all-games/a/"
        );
    }
}
