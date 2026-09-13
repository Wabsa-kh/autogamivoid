//! Offline verification of the game-page deep scrape against captured HTML
//! from real source pages. The fixtures are trimmed to the relevant markup.

use autogamivoid::models::SourceLabel;
use autogamivoid::scraper::extract_page_details;

/// SteamUnlocked game-page shape: hero chips, su-dl-primary button,
/// JSON-LD with fileSize, requirements list. Images are deliberately present
/// in the fixture to prove they are NOT extracted.
const STEAMUNLOCKED_PAGE: &str = r#"
<html><head>
<title>Blackthorn Arena: Reforged Free Download &raquo; SteamUnlocked</title>
<meta property="og:image" content="https://steamunlocked.org/wp-content/uploads/2026/01/Blackthorn-Arena-Reforged-Free-Download.jpg"/>
</head><body>
<span class="su-hchip su-hchip--genre">Simulation</span>
<span class="su-hchip su-hchip--size">28.11 GB</span>
<a class="su-dl-primary" href="https://uploadhaven.com/download/e72ee678de10d76bba8918d8a382f7e9" target="_blank" rel="noopener nofollow" aria-label="Download Blackthorn Arena: Reforged from UploadHaven">
<span class="su-dl-primary__text"> Download<span class="su-dl-primary__size">(28.11 GB)</span> </span></a>
<h2>System Requirements</h2>
<ul>
<li>Requires a 64-bit processor and operating system</li>
<li><strong>OS *:</strong> Windows 7/8/10 (64 bits)</li>
<li><strong>Processor:</strong> Intel Core i5-3450 (3.1 GHz) / AMD FX-6300 X6 (3.5 GHz)</li>
<li><strong>Memory:</strong> 8 GB RAM</li>
<li><strong>Graphics:</strong> 2 GB, GeForce GTX 660/Radeon HD 7870</li>
<li><strong>Storage:</strong> 30 GB available space</li>
</ul>
<script type="application/ld+json">{ "fileSize": "28.11 GB", "genre": [ "Simulation" ] }</script>
<img src="https://steamunlocked.org/wp-content/uploads/2026/01/Blackthorn-Arena-Reforged-crack.jpg"/>
</body></html>
"#;

/// Steamrip game-page shape: meta list (Genre/Developer/Game Size/Version)
/// and a MegaDB "DOWNLOAD HERE" anchor with a protocol-relative href.
const STEAMRIP_PAGE: &str = r#"
<html><head>
<title>1 Trait Escape Free Download (v1.15) &raquo; SteamRIP</title>
<meta property="og:image" content="https://steamrip.com/wp-content/uploads/2024/12/1-trait-escape-preinstalled-steamrip.jpg"/>
</head><body>
<ul>
<li><strong>Genre:</strong> Action</li>
<li><strong>Developer:</strong> Andrew Katz, Jeremy Collette</li>
<li><strong>Platform:</strong> PC</li>
<li><strong>Game Size: </strong>2.8 GB</li>
<li><strong>Version</strong>: v1.15 | Full Version</li>
<li><strong>OS</strong>: Windows&reg; 10 64-bit (latest Service Pack)</li>
<li><strong>Processor</strong>: Intel&reg; Core&trade; i3 or AMD Phenom&trade; X3 8650</li>
<li><strong>Memory</strong>: 6 GB RAM</li>
<li><strong>Graphics</strong>: NVIDIA&reg; GeForce&reg; GTX 600 series</li>
<li><strong>Storage</strong>: 10 GB available space</li>
</ul>
<a href="//megadb.net/aw0at8o3c964" target="_blank" rel="nofollow" class="shortc-button medium purple">DOWNLOAD HERE</a>
<img src="/wp-content/uploads/2024/12/1-trait-escape-screenshots-steamrip.jpg"/>
</body></html>
"#;

#[test]
fn steamunlocked_page_yields_direct_download_link() {
    let d = extract_page_details(
        STEAMUNLOCKED_PAGE,
        SourceLabel::Steamunlocked,
        "https://steamunlocked.org/blackthorn-arena-reforged-free-download/",
    );
    assert_eq!(
        d.download_urls.first().map(String::as_str),
        Some("https://uploadhaven.com/download/e72ee678de10d76bba8918d8a382f7e9"),
        "the su-dl-primary file-host link must be extracted"
    );
    assert_eq!(d.download_host.as_deref(), Some("Uploadhaven"));
    assert_eq!(d.file_size.as_deref(), Some("28.11 GB"));
    assert_eq!(d.genres, vec!["Simulation".to_string()]);
    let reqs = d.minimum.clone().expect("requirements extracted");
    assert!(reqs.contains("Windows 7/8/10"));
    assert!(reqs.contains("8 GB RAM"));
    assert_eq!(
        d.title.as_deref(),
        Some("Blackthorn Arena: Reforged Free Download")
    );
    assert!(!d.is_empty());
    assert_source_page_has_no_images(&d);
}

#[test]
fn steamrip_page_yields_megadb_link_and_metadata() {
    let d = extract_page_details(
        STEAMRIP_PAGE,
        SourceLabel::Steamrip,
        "https://steamrip.com/1-trait-escape-free-download/",
    );
    assert_eq!(
        d.download_urls.first().map(String::as_str),
        Some("https://megadb.net/aw0at8o3c964"),
        "protocol-relative MegaDB link must be absolutized"
    );
    assert_eq!(d.download_host.as_deref(), Some("Megadb"));
    assert_eq!(d.file_size.as_deref(), Some("2.8 GB"));
    assert_eq!(d.version.as_deref(), Some("1.15"));
    assert_eq!(d.developer.as_deref(), Some("Andrew Katz, Jeremy Collette"));
    assert_eq!(d.genres, vec!["Action".to_string()]);
    let reqs = d.minimum.clone().expect("requirements extracted");
    assert!(reqs.contains("Windows"));
    assert!(reqs.contains("6 GB RAM"));
    assert_source_page_has_no_images(&d);
}

#[test]
fn empty_page_produces_empty_details() {
    let d = extract_page_details(
        "<html><body>hi</body></html>",
        SourceLabel::Steamrip,
        "https://steamrip.com/x-free-download/",
    );
    assert!(d.download_urls.is_empty());
    assert!(d.file_size.is_none());
    assert!(d.minimum.is_none());
    assert!(d.is_empty());
}

/// Guard: SourcePageDetails must not carry image data at all, so no
/// steamrip/steamunlocked artwork can ever leak into a listing.
fn assert_source_page_has_no_images(d: &autogamivoid::models::SourcePageDetails) {
    let json = serde_json::to_string(d).unwrap();
    assert!(
        !json.contains("steamrip.com/wp-content"),
        "no steamrip images may be captured"
    );
    assert!(
        !json.contains("steamunlocked.org/wp-content"),
        "no steamunlocked images may be captured"
    );
}
