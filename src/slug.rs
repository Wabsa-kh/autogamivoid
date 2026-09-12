use heck::ToSnakeCase;

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

    if out.len() > 100 {
        out.truncate(100);
        while out.ends_with('-') {
            out.pop();
        }
    }

    if out.is_empty() {
        out.push_str("game");
    }

    out
}

#[cfg(test)]
mod tests {
    use super::slug_from_title;

    #[test]
    fn slug_keeps_stable_form() {
        assert_eq!(slug_from_title("  The  Longest  Journey  "), "the-longest-journey");
        assert_eq!(slug_from_title("Half-Life 2"), "half-life-2");
        assert_eq!(slug_from_title("Some Game [v2.0]"), "some-game-v20");
    }
}
