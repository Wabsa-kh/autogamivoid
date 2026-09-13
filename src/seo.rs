use crate::models::SourceLabel;

/// Everything SEO-related we attach to a gamivoid listing. Field names map to
/// the publishing guide: `seoTitle`, `seoDescription`, plain-text
/// `description` (30-10000 chars) and a safe-Markdown `article`.
#[derive(Clone, Debug, Default)]
pub struct SeoBundle {
    pub seo_title: String,
    pub seo_description: String,
    pub description: String,
    pub article_md: String,
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

const SEO_TITLE_MAX: usize = 120;
const SEO_DESC_MAX: usize = 320;

/// Build the full SEO bundle for one listing.
///
/// - `seo_title`: "{Title} Free Download PC Game (vX)" pattern, under 120 chars.
/// - `seo_description`: under 320 chars, exact query + version + developer + CTA.
/// - `description`: plain text (no HTML) for page metadata fallback.
/// - `article_md`: structured Markdown the API renders (About / Details /
///   Install steps). No em-dashes anywhere; ASCII punctuation only.
pub fn build_listing(input: &SeoInput<'_>) -> SeoBundle {
    let title = clean_title(input.title);
    let version = input.version.filter(|v| !v.trim().is_empty());

    SeoBundle {
        seo_title: seo_title(&title, version),
        seo_description: seo_description(&title, version, input),
        description: plain_description(&title, version, input),
        article_md: article_markdown(&title, version, input),
    }
}

fn clean_title(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Replace dashes that read badly in SEO copy with plain ASCII phrasing.
/// The guide-generated text should not contain em/en dashes at all, so this
/// doubles as a final safety net over every produced string.
fn ascii_punctuation(text: &str) -> String {
    text.replace(['\u{2013}', '\u{2014}'], "-")
        .replace('\u{2019}', "'")
        .replace(['\u{201c}', '\u{201d}'], "\"")
        .replace('\u{2026}', "...")
}

fn seo_title(title: &str, version: Option<&str>) -> String {
    let with_version = version
        .map(|v| format!("{title} Free Download PC Game (v{v})"))
        .unwrap_or_else(|| format!("{title} Free Download PC Game"));
    if with_version.chars().count() <= SEO_TITLE_MAX {
        return ascii_punctuation(&with_version);
    }

    let no_version = format!("{title} Free Download PC");
    if no_version.chars().count() <= SEO_TITLE_MAX {
        return ascii_punctuation(&no_version);
    }

    ascii_punctuation(&truncate_at_word(title, SEO_TITLE_MAX))
}

fn seo_description(title: &str, version: Option<&str>, input: &SeoInput<'_>) -> String {
    let mut desc = format!("Download {title} for free on PC. ");
    if let Some(v) = version {
        desc.push_str(&format!("Full version {v}, pre-installed and ready to play. "));
    } else {
        desc.push_str("Full game, pre-installed and ready to play. ");
    }
    if let Some(dev) = input.developer {
        push_if_fits(&mut desc, &format!("By {dev}. "), SEO_DESC_MAX);
    }
    if let Some(cat) = input.category {
        push_if_fits(&mut desc, &format!("{cat} game with a direct download link. "), SEO_DESC_MAX);
    } else {
        push_if_fits(&mut desc, "Direct download link included. ", SEO_DESC_MAX);
    }
    push_if_fits(&mut desc, "Install guide and system requirements on the page.", SEO_DESC_MAX);
    ascii_punctuation(desc.trim()).chars().take(SEO_DESC_MAX).collect()
}

fn plain_description(title: &str, version: Option<&str>, input: &SeoInput<'_>) -> String {
    let mut desc = format!(
        "{title} free download for PC. Get the full game, pre-installed and ready to play."
    );
    if let Some(v) = version {
        desc.push_str(&format!(" Current version: {v}."));
    }
    if let Some(blurb) = input.steam_blurb.map(clean_title).filter(|b| !b.is_empty()) {
        let blurb = ascii_punctuation(&blurb);
        let room = 900usize.saturating_sub(desc.chars().count());
        if room > 40 {
            let take: String = blurb.chars().take(room).collect();
            desc.push(' ');
            desc.push_str(&take);
        }
    } else if let Some(dev) = input.developer {
        desc.push_str(&format!(" Developed by {dev}."));
    }
    desc
}

fn push_if_fits(target: &mut String, piece: &str, max: usize) {
    if target.chars().count() + piece.chars().count() <= max {
        target.push_str(piece);
    }
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

// ---------------------------------------------------------------------------
// Markdown article
// ---------------------------------------------------------------------------

fn article_markdown(title: &str, version: Option<&str>, input: &SeoInput<'_>) -> String {
    let mut md = String::with_capacity(3000);

    md.push_str(&format!("## About {title}\n\n"));
    match input.steam_blurb.map(ascii_punctuation).filter(|s| !s.trim().is_empty()) {
        Some(blurb) => md.push_str(blurb.trim()),
        None => {
            let source = match input.source_label {
                SourceLabel::Steamrip => "Steamrip",
                SourceLabel::Steamunlocked => "SteamUnlocked",
            };
            md.push_str(&format!(
                "{title} is a full PC game release. This listing collects verified download details from {source} so you can get the complete game, pre-installed and ready to play."
            ));
        }
    }
    md.push_str("\n\n");

    md.push_str("## Game Details\n\n");
    let mut row = |label: &str, value: Option<&str>| {
        if let Some(v) = value.map(ascii_punctuation).filter(|v| !v.trim().is_empty()) {
            md.push_str(&format!("- **{label}:** {}\n", v.trim()));
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
    md.push('\n');

    md.push_str(&format!("## How to Download & Install {title}\n\n"));
    md.push_str("1. Click the **Download** button below and save the file.\n");
    md.push_str("2. Extract the archive if prompted (no extra software needed).\n");
    md.push_str("3. Run the setup and let it install. The game comes pre-installed and patched.\n");
    md.push_str(&format!("4. Launch {title} and play.\n\n"));

    let mut closing = format!(
        "This {title} PC download is the complete game"
    );
    if let Some(v) = version {
        closing.push_str(&format!(", updated to version {v}"));
    }
    if let Some(cat) = input.category {
        closing.push_str(&format!(" in the {} category", cat.to_lowercase()));
    }
    closing.push_str(&format!(". Bookmark this page for {title} updates, which are added automatically."));
    md.push_str(&ascii_punctuation(&closing));

    md
}

/// Shared install-step copy used both in the article and the guide's
/// `instructions` array field (max 20 steps, 1-1000 chars each).
pub fn install_steps(title: &str) -> Vec<String> {
    let t = ascii_punctuation(&clean_title(title));
    vec![
        "Click the Download button on this page and save the file.".to_string(),
        "Extract the archive if prompted (no extra software needed).".to_string(),
        "Run the setup and let it install. The game comes pre-installed and patched.".to_string(),
        format!("Launch {t} and play."),
    ]
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
    fn seo_title_matches_intent_and_length() {
        let tags = vec!["Action".to_string()];
        let seo = build_listing(&input("Doom", Some("1.0"), None, Some("id Software"), &tags, Some("Action")));
        assert_eq!(seo.seo_title, "Doom Free Download PC Game (v1.0)");
        assert!(seo.seo_title.chars().count() <= SEO_TITLE_MAX);
    }

    #[test]
    fn seo_description_under_limit_with_cta() {
        let tags = vec![];
        let seo = build_listing(&input(
            "Elden Ring",
            Some("1.10"),
            None,
            Some("FromSoftware"),
            &tags,
            Some("RPG"),
        ));
        assert!(seo.seo_description.chars().count() <= SEO_DESC_MAX);
        assert!(seo.seo_description.contains("Elden Ring"));
        assert!(seo.seo_description.to_lowercase().contains("download"));
    }

    #[test]
    fn article_is_markdown_without_html_or_dashes() {
        let tags = vec!["RPG".to_string()];
        let seo = build_listing(&input(
            "Doom",
            Some("1.0"),
            Some("Hell breaks loose."),
            Some("id Software"),
            &tags,
            Some("Action"),
        ));
        let body = &seo.article_md;
        assert!(body.contains("## About Doom"));
        assert!(body.contains("Hell breaks loose."));
        assert!(body.contains("- **Developer:** id Software"));
        assert!(body.contains("## How to Download & Install Doom"));
        assert!(!body.contains("<h2>"));
        assert!(!body.contains('\u{2013}'));
        assert!(!body.contains('\u{2014}'));
    }

    #[test]
    fn long_titles_get_truncated_seo_title() {
        let tags = vec![];
        let long = "A Very Long Game Title That Goes On And On And Eventually Exceeds One Hundred And Twenty Characters Which Forces The Generator To Truncate The Title At A Word Boundary";
        let seo = build_listing(&input(long, Some("1.0"), None, None, &tags, None));
        assert!(seo.seo_title.chars().count() <= SEO_TITLE_MAX);
    }

    #[test]
    fn description_is_plain_text_and_long_enough() {
        let tags = vec![];
        let seo = build_listing(&input("Short Game", None, None, None, &tags, None));
        assert!(!seo.description.contains('<'));
        assert!(seo.description.chars().count() >= 30, "must satisfy the published-game 30-char minimum");
    }

    #[test]
    fn smart_quotes_and_dashes_are_normalized() {
        assert_eq!(ascii_punctuation("a\u{2014}b"), "a-b");
        assert_eq!(ascii_punctuation("it\u{2019}s"), "it's");
    }
}
