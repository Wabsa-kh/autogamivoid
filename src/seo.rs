use crate::models::SourceLabel;

/// Everything SEO-related we attach to a gamivoid listing.
#[derive(Clone, Debug, Default)]
pub struct SeoBundle {
    pub meta_title: String,
    pub meta_description: String,
    pub meta_keywords: Vec<String>,
    pub description_html: String,
}

pub struct SeoInput<'a> {
    pub title: &'a str,
    pub version: Option<&'a str>,
    pub source_label: &'a SourceLabel,
    pub steam_blurb: Option<&'a str>,
    pub developer: Option<&'a str>,
    pub publisher: Option<&'a str>,
    pub release_date: Option<&'a str>,
    pub platforms: &'a [String],
    pub tags: &'a [String],
    pub category: Option<&'a str>,
}

const META_TITLE_MAX: usize = 65;
const META_DESC_MAX: usize = 155;

/// Build the full SEO bundle for one listing.
///
/// Meta title pattern matches high-intent search queries ("{game} free
/// download"), the meta description stays under 155 chars with a call to
/// action, and the body is a structured HTML listing (lead, about, details
/// table, install steps, closing keyword paragraph) that search engines can
/// index cleanly.
pub fn build_listing(input: &SeoInput<'_>) -> SeoBundle {
    let title = clean_title(input.title);
    let version = input.version.filter(|v| !v.trim().is_empty());

    SeoBundle {
        meta_title: meta_title(&title, version),
        meta_description: meta_description(&title, version, input),
        meta_keywords: meta_keywords(&title, input),
        description_html: description_html(&title, version, input),
    }
}

fn clean_title(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// Meta title
// ---------------------------------------------------------------------------

fn meta_title(title: &str, version: Option<&str>) -> String {
    let with_version = version
        .map(|v| format!("{title} Free Download PC (v{v})"))
        .unwrap_or_else(|| format!("{title} Free Download PC Game"));
    if with_version.chars().count() <= META_TITLE_MAX {
        return with_version;
    }

    let no_version = format!("{title} Free Download PC");
    if no_version.chars().count() <= META_TITLE_MAX {
        return no_version;
    }

    truncate_at_word(title, META_TITLE_MAX)
}

// ---------------------------------------------------------------------------
// Meta description
// ---------------------------------------------------------------------------

fn meta_description(title: &str, version: Option<&str>, input: &SeoInput<'_>) -> String {
    let mut desc = format!("Download {title} for free on PC. ");
    if let Some(v) = version {
        desc.push_str(&format!("Full version {v}, pre-installed. "));
    } else {
        desc.push_str("Full game, pre-installed. ");
    }
    if let Some(dev) = input.developer {
        push_if_fits(&mut desc, &format!("By {dev}. "), META_DESC_MAX);
    }
    if let Some(cat) = input.category {
        push_if_fits(&mut desc, &format!("{cat} game with a direct download link. "), META_DESC_MAX);
    } else {
        push_if_fits(&mut desc, "Direct download link. ", META_DESC_MAX);
    }
    if desc.trim_end().chars().count() < META_DESC_MAX - 30 {
        push_if_fits(&mut desc, "Safe, fast install guide included. ", META_DESC_MAX);
    }
    desc.trim().to_string()
}

fn push_if_fits(target: &mut String, piece: &str, max: usize) {
    if target.chars().count() + piece.chars().count() <= max {
        target.push_str(piece);
    }
}

// ---------------------------------------------------------------------------
// Meta keywords
// ---------------------------------------------------------------------------

fn meta_keywords(title: &str, input: &SeoInput<'_>) -> Vec<String> {
    let t = title.to_lowercase();
    let mut keywords = vec![
        t.clone(),
        format!("{t} free download"),
        format!("{t} pc download"),
        format!("download {t}"),
        format!("{t} full version"),
    ];
    for tag in input.tags.iter().take(4) {
        let tag = tag.trim().to_lowercase();
        keywords.push(format!("{t} {tag}"));
        keywords.push(tag);
    }
    if let Some(dev) = input.developer {
        keywords.push(dev.trim().to_lowercase());
    }
    keywords.sort();
    keywords.dedup();
    keywords.truncate(10);
    keywords
}

// ---------------------------------------------------------------------------
// Listing body (HTML)
// ---------------------------------------------------------------------------

fn description_html(title: &str, version: Option<&str>, input: &SeoInput<'_>) -> String {
    let t = escape_html(title);
    let mut html = String::with_capacity(2048);

    // Lead paragraph — the exact query users type, answered immediately.
    html.push_str(&format!(
        "<p><strong>{t} free download</strong> — get the full PC game, pre-installed and ready to play. \
         Click the download button below to grab {t}",
    ));
    if let Some(v) = version {
        html.push_str(&format!(" version {}", escape_html(v)));
    }
    html.push_str(" with a direct link, no waiting pages.</p>");

    // About section — Steam's official blurb when available.
    html.push_str(&format!("<h2>About {t}</h2>"));
    match input.steam_blurb.filter(|s| !s.trim().is_empty()) {
        Some(blurb) => html.push_str(&format!("<p>{}</p>", escape_html(blurb.trim()))),
        None => {
            let source = match input.source_label {
                SourceLabel::Steamrip => "Steamrip",
                SourceLabel::Steamunlocked => "SteamUnlocked",
            };
            html.push_str(&format!(
                "<p>{t} is a full PC game release collected from {source} and listed here with \
                 verified download details. This {t} download includes the complete game \
                 pre-installed, so you can install and play right away.</p>"
            ));
        }
    }

    // Game details table — structured data search engines love.
    html.push_str("<h2>Game Details</h2><ul>");
    let mut row = |label: &str, value: Option<&str>| {
        if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
            html.push_str(&format!(
                "<li><strong>{label}:</strong> {}</li>",
                escape_html(v.trim())
            ));
        }
    };
    row("Title", Some(title));
    row("Version", version);
    if !input.tags.is_empty() {
        let genres = input
            .tags
            .iter()
            .take(4)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        row("Genre", Some(&genres));
    }
    row("Developer", input.developer);
    row("Publisher", input.publisher);
    row("Release Date", input.release_date);
    if !input.platforms.is_empty() {
        let platform_str = input.platforms.join(", ");
        row("Platforms", Some(&platform_str));
    }
    html.push_str("</ul>");

    // Install steps — unique content on every page and genuinely useful.
    html.push_str(&format!("<h2>How to Download &amp; Install {t}</h2><ol>"));
    html.push_str("<li>Click the <strong>Download</strong> button below and save the file.</li>");
    html.push_str("<li>Extract the archive if prompted (no extra software needed).</li>");
    html.push_str("<li>Run the setup and let it install — the game comes pre-installed and patched.</li>");
    let launch = format!("<li>Launch {t} and play.</li>");
    html.push_str(&launch);
    html.push_str("</ol>");

    // Closing paragraph — one more natural keyword mention.
    let mut closing = format!(
        "This {t} PC download is the complete game, updated to the latest version"
    );
    if let Some(cat) = input.category {
        closing.push_str(&format!(" in the {} category", cat.to_lowercase()));
    }
    closing.push_str(&format!(". Bookmark this page — new {t} updates are added automatically."));
    html.push_str(&format!("<p>{}</p>", closing));

    html
}

fn truncate_at_word(s: &str, max: usize) -> String {
    let mut out = String::new();
    for word in s.split_whitespace() {
        let candidate_len = out.chars().count() + word.chars().count() + 1;
        if candidate_len > max && !out.is_empty() {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    static STEAMRIP: SourceLabel = SourceLabel::Steamrip;

    fn input<'a>(
        title: &'a str,
        version: Option<&'a str>,
        blurb: Option<&'a str>,
        developer: Option<&'a str>,
        tags: &'a [String],
        category: Option<&'a str>,
    ) -> SeoInput<'a> {
        let source_label = &STEAMRIP;
        let platforms: Vec<String> = vec!["Windows".to_string()];
        // Leak is fine in tests: tiny fixed data, process-lifetime.
        let platforms: &'static [String] = Box::leak(platforms.into_boxed_slice());
        SeoInput {
            title,
            version,
            source_label,
            steam_blurb: blurb,
            developer,
            publisher: None,
            release_date: Some("12 Mar, 2020"),
            platforms,
            tags,
            category,
        }
    }

    #[test]
    fn meta_title_matches_search_intent_and_length() {
        let tags = vec!["Action".to_string()];
        let seo = build_listing(&input("Doom", Some("1.0"), None, Some("id Software"), &tags, Some("Action")));
        assert_eq!(seo.meta_title, "Doom Free Download PC (v1.0)");
        assert!(seo.meta_title.chars().count() <= META_TITLE_MAX);
    }

    #[test]
    fn meta_description_under_limit_with_call_to_action() {
        let tags = vec![];
        let seo = build_listing(&input(
            "Elden Ring",
            Some("1.10"),
            None,
            Some("FromSoftware"),
            &tags,
            Some("RPG"),
        ));
        assert!(seo.meta_description.chars().count() <= META_DESC_MAX);
        assert!(seo.meta_description.contains("Elden Ring"));
        assert!(seo.meta_description.to_lowercase().contains("download"));
    }

    #[test]
    fn keywords_cover_intent_variants() {
        let tags = vec!["Action".to_string(), "Open World".to_string()];
        let seo = build_listing(&input("Doom", None, None, None, &tags, None));
        assert!(seo.meta_keywords.contains(&"doom free download".to_string()));
        assert!(seo.meta_keywords.contains(&"doom action".to_string()));
        assert!(seo.meta_keywords.len() <= 10);
    }

    #[test]
    fn body_has_headings_details_and_install_steps() {
        let tags = vec!["RPG".to_string()];
        let seo = build_listing(&input(
            "Doom",
            Some("1.0"),
            Some("Hell breaks loose."),
            Some("id Software"),
            &tags,
            Some("Action"),
        ));
        let body = &seo.description_html;
        assert!(body.contains("<h2>About Doom</h2>"));
        assert!(body.contains("Hell breaks loose."));
        assert!(body.contains("<li><strong>Developer:</strong> id Software</li>"));
        assert!(body.contains("How to Download &amp; Install Doom"));
        assert!(body.contains("version 1.0"));
    }

    #[test]
    fn body_escapes_html_in_names() {
        let tags = vec![];
        let seo = build_listing(&input("Rock & Roll <Racer>", None, None, None, &tags, None));
        assert!(seo.description_html.contains("Rock &amp; Roll &lt;Racer&gt;"));
        assert!(!seo.description_html.contains("<Racer>"));
    }

    #[test]
    fn long_titles_get_truncated_meta_title() {
        let tags = vec![];
        let long = "A Very Long Game Title That Goes On And On And Eventually Exceeds Sixty Five Characters Easily";
        let seo = build_listing(&input(long, Some("1.0"), None, None, &tags, None));
        assert!(seo.meta_title.chars().count() <= META_TITLE_MAX);
    }
}
