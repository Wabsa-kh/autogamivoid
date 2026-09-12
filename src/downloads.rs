use crate::models::{DownloadCandidate, SourceLabel};

/// Decide which download candidate should be published for a matched game.
pub fn choose_download(candidates: &[DownloadCandidate]) -> DownloadCandidate {
    let mut best: Option<DownloadCandidate> = None;
    for candidate in candidates {
        best = Some(match best.take() {
            None => candidate.clone(),
            Some(prev) => {
                if candidate_is_better(candidate, &prev) {
                    candidate.clone()
                } else {
                    prev
                }
            }
        });
    }
    best.unwrap_or_else(|| DownloadCandidate {
        url: String::new(),
        source_label: SourceLabel::Steamrip,
        version: None,
        notes: None,
    })
}

fn candidate_is_better(a: &DownloadCandidate, b: &DownloadCandidate) -> bool {
    let a_has = version_present(&a.version);
    let b_has = version_present(&b.version);

    if a_has && !b_has {
        return true;
    }
    if b_has && !a_has {
        return false;
    }
    if a_has && b_has {
        if let (Some(av), Some(bv)) = (&a.version, &b.version) {
            if version_newer(av, bv) {
                return true;
            }
            if version_newer(bv, av) {
                return false;
            }
        }
    }
    if a.source_label == SourceLabel::Steamrip && b.source_label != SourceLabel::Steamrip {
        return true;
    }
    if b.source_label == SourceLabel::Steamrip && a.source_label != SourceLabel::Steamrip {
        return false;
    }
    false
}

fn version_present(v: &Option<String>) -> bool {
    v.as_ref().is_some_and(|s| !s.trim().is_empty())
}

fn version_newer(av: &str, bv: &str) -> bool {
    parse_version(av) > parse_version(bv)
}

fn parse_version(raw: &str) -> Vec<u64> {
    raw.split(|c: char| !c.is_ascii_digit())
        .filter_map(|s| s.parse().ok())
        .collect()
}

/// Convert enriched downloads into the gamivoid payload representation.
pub fn downloads_to_patch_field(downloads: &[DownloadCandidate]) -> Vec<serde_json::Value> {
    downloads
        .iter()
        .map(|d| {
            let mut map = serde_json::Map::new();
            map.insert("url".into(), serde_json::Value::String(d.url.clone()));
            map.insert(
                "source".into(),
                serde_json::Value::String(d.source_label.to_string()),
            );
            if let Some(v) = &d.version {
                map.insert("version".into(), serde_json::Value::String(v.clone()));
            }
            if let Some(n) = &d.notes {
                map.insert("notes".into(), serde_json::Value::String(n.clone()));
            }
            serde_json::Value::Object(map)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_newer_version_and_steamrip_tiebreak() {
        let older = DownloadCandidate {
            url: "https://a.test".into(),
            source_label: SourceLabel::Steamrip,
            version: Some("1.0".into()),
            notes: None,
        };
        let newer_unlocked = DownloadCandidate {
            url: "https://b.test".into(),
            source_label: SourceLabel::Steamunlocked,
            version: Some("2.0".into()),
            notes: None,
        };
        let chosen = choose_download(&[older, newer_unlocked]);
        assert_eq!(chosen.source_label, SourceLabel::Steamunlocked);
        assert_eq!(chosen.version.as_deref(), Some("2.0"));
    }

    #[test]
    fn empty_candidates_fall_back_to_placeholder() {
        let chosen = choose_download(&[]);
        assert!(chosen.url.is_empty());
    }
}
