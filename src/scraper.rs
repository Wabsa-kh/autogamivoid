use crate::config::SourceEndpoint;
use crate::models::{SourceGame, SourceLabel, SourcePageDetails};
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
// Game-page deep scrape — the actual download link + real metadata
// ---------------------------------------------------------------------------

/// Fetch one game page and extract what the listing anchors never show:
///
/// - the *actual* download link (SteamUnlocked: `su-dl-primary` anchor to
///   uploadhaven.com; Steamrip: file-host anchor inside the entry content,
///   e.g. megadb.net) — the "click the button and get the real link" fix;
/// - the archive size ("Download(28.11 GB)" / "Game Size: 2.8 GB"),
/// - the version marker ("v1.15 | Full Version"),
/// - developer / publisher / genre chips, system requirements.
///
/// No images are extracted: all artwork must come from Steam's CDN.
///
/// Anything that fails to parse simply stays None/empty: the caller falls
/// back to listing-level data.
pub async fn scrape_game_page(
    cfg: &SourceEndpoint,
    label: SourceLabel,
    page_url: &str,
) -> anyhow::Result<SourcePageDetails> {
    if cfg.base_url.is_empty() {
        anyhow::bail!("{} base_url is not configured", label);
    }
    let client = http_client()?;
    let html = fetch_text(&client, cfg, page_url).await?;
    Ok(extract_page_details(&html, label, page_url))
}

/// Parse a fetched game page into SourcePageDetails (images excluded on
/// purpose: artwork must come from Steam's CDN, never the source sites).
pub fn extract_page_details(html: &str, label: SourceLabel, _page_url: &str) -> SourcePageDetails {
    let flat = html.replace('\n', " ");

    let mut details = SourcePageDetails {
        download_urls: extract_download_urls(&flat, &label),
        download_host: None,
        version: extract_page_version(&flat),
        file_size: extract_file_size(&flat),
        developer: extract_meta_field(&flat, &["Developer:", "Developer"]),
        publisher: extract_meta_field(&flat, &["Publisher:", "Publisher"]),
        genres: extract_genres(&flat, &label),
        minimum: extract_requirements(&flat),
        title: extract_page_title(&flat),
    };

    // The host name comes from the first extracted download URL.
    if let Some(first) = details.download_urls.first() {
        details.download_host = host_label(first);
    }
    details
}

/// Known file hosts the two sites hand out. Order matters for display only.
const FILE_HOSTS: [&str; 16] = [
    "uploadhaven.com",
    "megadb.net",
    "gofile.io",
    "buzzheavier.com",
    "1fitchier.com",
    "1fichier.com",
    "datanodes.to",
    "usersdrive.com",
    "bowfile.com",
    "multiup.io",
    "multiup.org",
    "krakenfiles.com",
    "pixeldrain.com",
    "mega.nz",
    "qdembed.com",
    "send.cm",
];

fn host_label(url: &str) -> Option<String> {
    for host in FILE_HOSTS {
        if url.contains(host) {
            let name = host.split('.').next()?;
            let mut c = name.chars();
            return Some(
                c.next()?.to_uppercase().collect::<String>() + c.as_str(),
            );
        }
    }
    None
}

/// Pull download URLs out of a game page:
/// - SteamUnlocked: the primary button anchor (`su-dl-primary`, or any
///   uploadhaven.com/download/... href);
/// - Steamrip: anchors to known file hosts inside the entry content.
fn extract_download_urls(flat: &str, label: &SourceLabel) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    fn push_url(urls: &mut Vec<String>, seen: &mut std::collections::HashSet<String>, url: &str) {
        let url = url.trim().to_string();
        if url.starts_with("https://")
            || url.starts_with("http://")
            || url.starts_with("//")
        {
            let absolute = if let Some(rest) = url.strip_prefix("//") {
                format!("https://{rest}")
            } else {
                url
            };
            if seen.insert(absolute.clone()) {
                urls.push(absolute);
            }
        }
    }

    // Primary download button (SteamUnlocked renders `class="su-dl-primary"`).
    if let Some(idx) = flat.find("su-dl-primary") {
        let window = &flat[idx..(idx + 600).min(flat.len())];
        if let Some(href) = extract_href(window) {
            push_url(&mut urls, &mut seen, &href);
        }
    }

    // Any anchor pointing at a known file host (both sites). Site pages of
    // those hosts (register/login/premium/...) are never download links.
    for (href, _) in iter_anchor_tags(flat) {
        let lower = href.to_lowercase();
        if FILE_HOSTS.iter().any(|h| lower.contains(h)) && !is_host_navigation_url(&lower) {
            push_url(&mut urls, &mut seen, href);
        }
    }

    // Generic anchors literally named download (Steamrip fallbacks like
    // "DOWNLOAD HERE" pointing at an unknown host).
    if urls.is_empty() {
        for (href, text) in iter_anchor_tags(flat) {
            let t = text.to_lowercase();
            if (t.contains("download") || t.contains("get game")) && !href.contains('#') {
                let lower = href.to_lowercase();
                let on_site = lower.contains("steamrip.com")
                    || lower.contains("steamunlocked");
                if !on_site {
                    push_url(&mut urls, &mut seen, href);
                }
            }
        }
    }

    let _ = label;
    urls
}

/// True for file-host pages that are NOT download endpoints (registration,
/// login, premium upsells, legal pages). These anchor every file-host site
/// and must never be listed as download mirrors.
fn is_host_navigation_url(lower_url: &str) -> bool {
    const NAV_SEGMENTS: [&str; 14] = [
        "/account", "/register", "/login", "/signin", "/signup", "/premium", "/upgrade",
        "/support", "/faq", "/dmca", "/contact", "/report", "/privacy", "/terms",
    ];
    NAV_SEGMENTS.iter().any(|seg| lower_url.contains(seg))
}

/// Extract the href="..." value from an HTML fragment.
fn extract_href(fragment: &str) -> Option<String> {
    let idx = fragment.find("href=")?;
    let rest = &fragment[idx + 5..];
    leading_quoted_value(rest)
}

/// Read the quoted string at the start of a fragment: `"https://.."` ->
/// `https://..`.
fn leading_quoted_value(fragment: &str) -> Option<String> {
    let rest = fragment.trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let end = rest[1..].find(quote)? + 1;
    Some(rest[1..end].to_string())
}

/// Version marker on the game page: "v1.15 | Full Version" (Steamrip meta
/// list) or the "(v1.4)" suffix already in the page title.
fn extract_page_version(flat: &str) -> Option<String> {
    if let Some(pos) = flat.find(">Version<") {
        let window = &flat[pos..(pos + 300).min(flat.len())];
        if let Some(end) = window.find("</li>") {
            if let Some(v) = version_from_scraped_text(&strip_tags(&window[..end])) {
                return Some(v);
            }
        }
    }
    if let Some(title) = extract_page_title(flat) {
        if let Some(v) = version_from_title(&title) {
            return Some(v);
        }
    }
    None
}

/// Pull a bare version like "1.15" out of scraped text such as
/// "Version: v1.15 | Full Version" or "Version: v1.15".
fn version_from_scraped_text(raw: &str) -> Option<String> {
    let after_label = raw.trim().trim_start_matches('>').trim();
    let cleaned = match after_label.split_once(':') {
        Some((_, value)) => value.trim(),
        None => after_label,
    };
    let head = cleaned.split('|').next().unwrap_or(cleaned).trim();
    let digits: String = head
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

/// Archive size: SteamUnlocked hero chip `su-hchip--size">28.11 GB` or the
/// button text "Download(28.11 GB)"; Steamrip "Game Size: </strong>2.8 GB".
fn extract_file_size(flat: &str) -> Option<String> {
    for marker in ["su-hchip--size\">", "Game Size: </strong>"] {
        if let Some(idx) = flat.find(marker) {
            let start = idx + marker.len();
            let window = &flat[start..(start + 60).min(flat.len())];
            let end = window.find('<').unwrap_or(window.len());
            let raw = window[..end].trim();
            if looks_like_size(raw) {
                return Some(raw.to_string());
            }
        }
    }
    // JSON-LD fileSize field.
    if let Some(idx) = flat.find("\"fileSize\":") {
        let window = &flat[idx..(idx + 80).min(flat.len())];
        let inner: String = window.chars().skip("\"fileSize\":".len()).collect();
        let inner = inner.trim_start();
        if let Some(rest) = inner.strip_prefix('"') {
            if let Some(end) = rest.find('"') {
                let raw = rest[..end].trim();
                if looks_like_size(raw) {
                    return Some(raw.to_string());
                }
            }
        }
    }
    // Button text "Download(28.11 GB)".
    if let Some(idx) = flat.find("Download(") {
        let window = &flat[idx..(idx + 60).min(flat.len())];
        if let Some(close) = window.find(')') {
            let raw = window["Download(".len()..close].trim();
            if looks_like_size(raw) {
                return Some(raw.to_string());
            }
        }
    }
    None
}

fn looks_like_size(s: &str) -> bool {
    let lower = s.to_lowercase();
    (lower.contains("gb") || lower.contains("mb") || lower.contains("kb"))
        && lower
            .chars()
            .any(|c| c.is_ascii_digit())
        && lower.len() <= 20
}

/// "Developer:</strong> Andrew Katz, Jeremy Collette" style fields from the
/// Steamrip meta list. Tolerates "Developer" (no colon) too.
fn extract_meta_field(flat: &str, labels: &[&str]) -> Option<String> {
    for label in labels {
        let needle = format!(">{label}");
        let Some(idx) = flat.find(&needle) else { continue };
        let window = &flat[idx..(idx + 300).min(flat.len())];
        let Some(end) = window.find("</li>").or_else(|| window.find("</p>")) else {
            continue;
        };
        let raw = strip_tags(&window[..end]);
        // " >Developer: Andrew Katz " -> "Andrew Katz" (the leading '>' is
        // the tail of the enclosing <strong>/<li> tag).
        let raw = raw
            .trim()
            .trim_start_matches('>')
            .trim()
            .split_once(':')
            .map(|(_, value)| value.trim())
            .unwrap_or("");
        let raw = raw.split('|').next().unwrap_or(raw).trim();
        if !raw.is_empty() && raw.chars().count() <= 150 {
            return Some(raw.to_string());
        }
    }
    None
}

/// Genre chips: SteamUnlocked `su-hchip--genre">Simulation`; Steamrip
/// "Genre:</strong> Action" meta entry.
fn extract_genres(flat: &str, label: &SourceLabel) -> Vec<String> {
    let mut genres = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push = |raw: &str| {
        let raw = raw.trim();
        if !raw.is_empty() && raw.chars().count() <= 60 && seen.insert(raw.to_lowercase()) {
            genres.push(raw.to_string());
        }
    };

    if *label == SourceLabel::Steamunlocked {
        for part in flat.split("su-hchip--genre\">") {
            if part.is_empty() {
                continue;
            }
            let end = part.find('<').unwrap_or(part.len());
            let value = decode_entities(&part[..end]);
            if !value.contains("games") && !value.contains("http") {
                push(&value);
            }
            // Only chips before the page body (avoid capturing article text).
            if flat.find(part).map(|p| p > 4000).unwrap_or(true) {
                break;
            }
        }
    } else {
        if let Some(field) = extract_meta_field(flat, &["Genre:", "Genre"]) {
            for g in field.split(',') {
                push(g);
            }
        }
    }
    genres
}

/// System requirements list: "<strong>OS</strong>: ..." entries (Steamrip),
/// or the "System Requirements" section list (SteamUnlocked).
fn extract_requirements(flat: &str) -> Option<String> {
    let window_start = flat
        .find("System Requirements")
        .or_else(|| flat.find("<strong>OS"))
        .or_else(|| flat.find("<strong>OS</strong>"))?;
    let window = &flat[window_start..(window_start + 3500).min(flat.len())];
    let end = window.find("</ul>").or_else(|| window.find("</ol>"))?;
    let list_html = &window[..end];

    let mut lines = Vec::new();
    for li in list_html.split("<li>") {
        let Some(rest) = li.split_once("</li>") else { continue };
        let text = decode_entities(&strip_tags(rest.0));
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        lines.push(text.to_string());
    }
    let text = lines.join("\n");
    if text.chars().count() >= 10 {
        Some(text)
    } else {
        None
    }
}

/// Strip tags conservatively: remove <strong>/<em>/<span>/<a> wrappers and
/// any remaining <...> fragments.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        match after.find('>') {
            Some(close) => rest = &after[close + 1..],
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// <title> without the site suffix ("Blackthorn Arena: Reforged Free Download").
fn extract_page_title(flat: &str) -> Option<String> {
    let start = flat.find("<title>")? + 7;
    let end = flat[start..].find("</title>")? + start;
    // Decode entities FIRST (&raquo; -> ») so the suffix split always hits.
    let raw = decode_entities(&strip_tags(&flat[start..end]));
    let mut cleaned = raw.clone();
    for marker in ["\u{00bb} SteamRIP", "\u{00bb} SteamUnlocked"] {
        if let Some(idx) = cleaned.find(marker) {
            cleaned = cleaned[..idx].trim().to_string();
        }
    }
    let cleaned = cleaned.trim().to_string();
    if cleaned.len() < 2 {
        return None;
    }
    Some(cleaned)
}

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
    let mut text = decode_entities(raw);

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

/// Decode HTML entities in scraped anchor text before any further processing,
/// so titles like "Birds Aren&#039;t Real" produce clean slugs.
pub fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < text.len() {
        if bytes[i] == b'&' {
            let rest = &text[i..];
            let decoded = if let Some(r) = rest.strip_prefix("&#039;") {
                let _ = r;
                Some(('\'', 6usize))
            } else if let Some(r) = rest.strip_prefix("&#39;") {
                let _ = r;
                Some(('\x27', 5))
            } else if let Some(r) = rest.strip_prefix("&#x27;") {
                let _ = r;
                Some(('\'', 6))
            } else if let Some(r) = rest.strip_prefix("&quot;") {
                let _ = r;
                Some(('"', 6))
            } else if let Some(r) = rest.strip_prefix("&amp;") {
                let _ = r;
                Some(('&', 5))
            } else if let Some(r) = rest.strip_prefix("&lt;") {
                let _ = r;
                Some(('<', 4))
            } else if let Some(r) = rest.strip_prefix("&gt;") {
                let _ = r;
                Some(('>', 4))
            } else if let Some(r) = rest.strip_prefix("&nbsp;") {
                let _ = r;
                Some((' ', 6))
            } else if let Some(r) = rest.strip_prefix("&raquo;") {
                let _ = r;
                Some(('»', 7))
            } else if let Some(r) = rest.strip_prefix("&laquo;") {
                let _ = r;
                Some(('«', 7))
            } else if let Some(r) = rest.strip_prefix("&eacute;") {
                let _ = r;
                Some(('é', 8))
            } else if let Some(r) = rest.strip_prefix("&#8211;") {
                let _ = r;
                Some(('-', 7))
            } else if let Some(r) = rest.strip_prefix("&#8212;") {
                let _ = r;
                Some(('-', 7))
            } else {
                None
            };
            if let Some((ch, len)) = decoded {
                out.push(ch);
                i += len;
                continue;
            }
            out.push('&');
            i += 1;
        } else {
            let ch = text[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out.trim().to_string()
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
    fn filehost_navigation_pages_are_not_downloads() {
        let html = r#"
            <a class="su-dl-primary" href="https://uploadhaven.com/download/abc123">Download</a>
            <a href="https://uploadhaven.com/account/register">Register</a>
            <a href="https://megadb.net/login">Login</a>
            <a href="https://megadb.net/dmca">DMCA</a>
        "#;
        let flat = html.replace('\n', " ");
        let urls = extract_download_urls(&flat, &SourceLabel::Steamunlocked);
        assert_eq!(urls, vec!["https://uploadhaven.com/download/abc123".to_string()]);
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

#[cfg(test)]
mod entity_tests {
    use super::*;

    #[test]
    fn decodes_common_entities_in_titles() {
        assert_eq!(decode_entities("Birds Aren&#039;t Real"), "Birds Aren't Real");
        assert_eq!(decode_entities("Rock &amp; Roll"), "Rock & Roll");
        assert_eq!(decode_entities("A&quot;B"), "A\"B");
        assert_eq!(decode_entities("Caf&eacute;"), "Café");
        assert_eq!(decode_entities("x&#39;y"), "x'y");
        assert_eq!(decode_entities("x&#039;y"), "x'y");
        assert_eq!(decode_entities("plain title"), "plain title");
    }
}
