use heck::ToSnakeCase;

/// Gamivoid SEO convention: every game slug ends with "-free-download"
/// (e.g. "blackthorn-arena-free-download"), matching the "{Title} Free
/// Download" page titles. Keeps URLs keyword-rich and consistent.
const SLUG_SUFFIX: &str = "free-download";
/// Guide cap for the slug field.
const SLUG_MAX: usize = 100;

pub fn slug_from_title(title: &str) -> String {
    let normalized = title
        .trim()
        .to_lowercase()
        .replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', '#', '[', ']'], "-");

    let words: Vec<&str> = normalized
        .split(|c: char| c.is_whitespace() || c == '-' || c == '_')
        .filter(|s| !s.is_empty())
        .collect();

    let mut out = String::new();
    for (i, word) in words.iter().enumerate() {
        if i > 0 {
            out.push('-');
        }
        let cleaned: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
        if cleaned.is_empty() {
            continue;
        }
        out.push_str(&cleaned.to_snake_case());
    }

    if out.is_empty() {
        out.push_str("game");
    }

    // Reserve room for the suffix, then trim the base at the cap.
    let budget = SLUG_MAX - SLUG_SUFFIX.len() - 1; // '-' + suffix
    if out.len() > budget {
        out.truncate(budget);
        while out.ends_with('-') {
            out.pop();
        }
    }

    if !out.ends_with(SLUG_SUFFIX) {
        out.push('-');
        out.push_str(SLUG_SUFFIX);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_keeps_stable_form_with_free_download_suffix() {
        assert_eq!(
            slug_from_title("  The  Longest  Journey  "),
            "the-longest-journey-free-download"
        );
        assert_eq!(slug_from_title("Half-Life 2"), "half-life-2-free-download");
        assert_eq!(
            slug_from_title("Some Game [v2.0]"),
            "some-game-v20-free-download"
        );
    }

    #[test]
    fn slug_respects_100_char_cap_including_suffix() {
        let long = "A Very Long Game Title That Goes On And On And Eventually Exceeds The One Hundred Character Slug Limit Which Forces Truncation";
        let slug = slug_from_title(long);
        assert!(slug.chars().count() <= SLUG_MAX);
        assert!(slug.ends_with("-free-download"));
    }

    #[test]
    fn slug_never_suffixes_twice() {
        assert_eq!(
            slug_from_title("Game Free Download"),
            "game-free-download"
        );
    }

    #[test]
    fn empty_title_still_produces_valid_slug() {
        let slug = slug_from_title("###");
        assert_eq!(slug, "game-free-download");
    }
}
