use crate::models::{DownloadCandidate, MatchedGame, SourceGame, SourceLabel};

pub fn normalize_title(title: &str) -> String {
    let lower = title.to_lowercase();
    let cleaned: String = lower
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Strip trailing version-like noise and marketing suffixes for matching only.
pub fn strip_version_noise(title: &str) -> String {
    let s = normalize_title(title);
    let trimmed = s.trim();
    if trimmed.ends_with(']') {
        if let Some(open) = trimmed.rfind('[') {
            let candidate = trimmed[..open].trim_end();
            if !candidate.is_empty() {
                return stripped_suffixes(candidate);
            }
        }
    }
    stripped_suffixes(trimmed)
}

fn stripped_suffixes(s: &str) -> String {
    let patterns = [
        " free download",
        " full version",
        " download",
        " pc game",
        " game",
        " pc",
        " windows",
        " full",
        " cracked",
        " repack",
        " torrent",
    ];
    let mut best = s.to_string();
    for pat in patterns {
        while let Some(end) = best.rfind(pat) {
            let candidate = best[..end].trim_end().to_string();
            if candidate.is_empty() {
                break;
            }
            best = candidate;
        }
    }
    best
}

/// Simple normalized similarity ratio in [0, 1].
pub fn title_similarity(a: &str, b: &str) -> f64 {
    let a = strip_version_noise(a);
    let b = strip_version_noise(b);
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a.starts_with(&b) || b.starts_with(&a) {
        return 0.9;
    }

    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let prefix_len = a_chars
        .iter()
        .zip(b_chars.iter())
        .take_while(|(x, y)| x == y)
        .count();
    let suffix_len = a_chars
        .iter()
        .rev()
        .zip(b_chars.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let shorter_len = a_chars.len().min(b_chars.len());
    let matched = (prefix_len + suffix_len).min(shorter_len);
    (matched as f64 / shorter_len as f64 * 0.95).min(0.85)
}

const MATCH_THRESHOLD: f64 = 0.85;

pub fn match_sources(
    steamrip: Vec<SourceGame>,
    steamunlocked: Vec<SourceGame>,
) -> Vec<MatchedGame> {
    if steamrip.len() * steamunlocked.len() > 4_000_000 {
        // O(n*m) pairing is too slow for full catalogs; bucket by first char.
        return match_sources_bucketed(steamrip, steamunlocked);
    }
    match_sources_exhaustive(steamrip, steamunlocked)
}

/// Full-catalog variant: bucket both sides by their first alphanumeric char
/// (after noise-stripping) so matching cost is per-bucket, not global.
pub fn match_sources_bucketed(
    steamrip: Vec<SourceGame>,
    steamunlocked: Vec<SourceGame>,
) -> Vec<MatchedGame> {
    use std::collections::HashMap;

    fn bucket_of(title: &str) -> char {
        crate::matching::strip_version_noise(title)
            .chars()
            .find(|c| c.is_ascii_alphanumeric())
            .unwrap_or('#')
    }

    let mut sr_buckets: HashMap<char, Vec<SourceGame>> = HashMap::new();
    for game in steamrip {
        sr_buckets
            .entry(bucket_of(&game.title))
            .or_default()
            .push(game);
    }
    let mut su_buckets: HashMap<char, Vec<SourceGame>> = HashMap::new();
    for game in steamunlocked {
        su_buckets
            .entry(bucket_of(&game.title))
            .or_default()
            .push(game);
    }

    let mut matched: Vec<MatchedGame> = Vec::new();
    let keys: Vec<char> = sr_buckets.keys().copied().collect();
    for key in keys {
        let Some(su_list) = su_buckets.remove(&key) else {
            continue;
        };
        let sr_list = sr_buckets.remove(&key).unwrap_or_default();
        matched.extend(match_sources_exhaustive(sr_list, su_list));
    }
    // Leftovers: single-source entries with no counterpart bucket.
    for (_, list) in sr_buckets {
        matched.extend(list.into_iter().map(single));
    }
    for (_, list) in su_buckets {
        matched.extend(list.into_iter().map(single));
    }
    matched
}

fn match_sources_exhaustive(
    steamrip: Vec<SourceGame>,
    steamunlocked: Vec<SourceGame>,
) -> Vec<MatchedGame> {
    let mut matched: Vec<MatchedGame> = Vec::new();
    let mut used_steamrip = vec![false; steamrip.len()];
    let mut used_steamunlocked = vec![false; steamunlocked.len()];

    // Pair obvious title matches first (greedy best-similarity pairing).
    for i in 0..steamrip.len() {
        if used_steamrip[i] {
            continue;
        }
        let mut best_j: Option<usize> = None;
        let mut best_sim = 0.0_f64;
        for j in 0..steamunlocked.len() {
            if used_steamunlocked[j] {
                continue;
            }
            let sim = title_similarity(&steamrip[i].title, &steamunlocked[j].title);
            if sim >= MATCH_THRESHOLD && sim > best_sim {
                best_sim = sim;
                best_j = Some(j);
            }
        }
        if let Some(j) = best_j {
            used_steamrip[i] = true;
            used_steamunlocked[j] = true;
            let sr = steamrip[i].clone();
            let su = steamunlocked[j].clone();
            matched.push(merge_candidates(vec![sr, su]));
        }
    }

    // Remaining unpaired entries become single-source matched games.
    for (i, used) in used_steamrip.iter().enumerate() {
        if !used {
            matched.push(single(steamrip[i].clone()));
        }
    }
    for (j, used) in used_steamunlocked.iter().enumerate() {
        if !used {
            matched.push(single(steamunlocked[j].clone()));
        }
    }

    // Final pass: merge games that collapsed to the same normalized title.
    dedupe_by_normalized(&mut matched);
    matched
}

fn single(game: SourceGame) -> MatchedGame {
    let normalized_title = normalize_title(&game.title);
    let raw_title = game.title.clone();
    let best: DownloadCandidate = game.clone().into();
    MatchedGame {
        normalized_title,
        raw_title,
        candidates: vec![game],
        best_download: best,
    }
}

fn merge_candidates(cands: Vec<SourceGame>) -> MatchedGame {
    let raw_title = cands
        .iter()
        .find(|c| c.source_label == SourceLabel::Steamrip)
        .or_else(|| cands.first())
        .map(|c| c.title.clone())
        .unwrap_or_default();
    let best_download = cands
        .iter()
        .max_by(|a, b| {
            if pick_rank(a) >= pick_rank(b) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Less
            }
        })
        .map(|g| g.clone().into())
        .unwrap_or(DownloadCandidate {
            url: String::new(),
            source_label: SourceLabel::Steamrip,
            version: None,
            notes: None,
        });
    MatchedGame {
        normalized_title: normalize_title(&raw_title),
        raw_title,
        candidates: cands,
        best_download,
    }
}

/// Higher is a better download pick: versioned beats unversioned, then Steamrip.
fn pick_rank(game: &SourceGame) -> u32 {
    let has_version = game.version.as_ref().is_some_and(|v| !v.trim().is_empty());
    let steamrip_bonus = (game.source_label == SourceLabel::Steamrip) as u32;
    (has_version as u32 * 2) + steamrip_bonus
}

fn dedupe_by_normalized(matched: &mut Vec<MatchedGame>) {
    let mut by_key: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut keep: Vec<MatchedGame> = Vec::with_capacity(matched.len());
    for entry in matched.drain(..) {
        let key = entry.normalized_title.clone();
        match by_key.get(&key) {
            Some(&idx) => {
                let first = &mut keep[idx];
                for candidate in entry.candidates {
                    if !first
                        .candidates
                        .iter()
                        .any(|existing| existing.download_url == candidate.download_url)
                    {
                        first.candidates.push(candidate);
                    }
                }
                first.best_download = best_download_of(&first.candidates);
            }
            None => {
                by_key.insert(key, keep.len());
                keep.push(entry);
            }
        }
    }
    *matched = keep;
}

fn best_download_of(cands: &[SourceGame]) -> DownloadCandidate {
    cands
        .iter()
        .max_by(|a, b| {
            if pick_rank(a) >= pick_rank(b) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Less
            }
        })
        .map(|g| g.clone().into())
        .unwrap_or(DownloadCandidate {
            url: String::new(),
            source_label: SourceLabel::Steamrip,
            version: None,
            notes: None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(title: &str, url: &str, version: Option<&str>, label: SourceLabel) -> SourceGame {
        SourceGame {
            title: title.into(),
            download_url: url.into(),
            version: version.map(str::to_string),
            notes: None,
            source_label: label,
        }
    }

    #[test]
    fn matches_identical_titles() {
        let sr = game("Half-Life 2", "https://a.test", Some("2.0"), SourceLabel::Steamrip);
        let su = game("Half-Life 2", "https://b.test", None, SourceLabel::Steamunlocked);
        let matched = match_sources(vec![sr], vec![su]);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].candidates.len(), 2);
        assert_eq!(matched[0].best_download.source_label, SourceLabel::Steamrip);
    }

    #[test]
    fn matches_across_version_suffixes() {
        let sr = game("Elden Ring", "https://a.test", Some("1.10"), SourceLabel::Steamrip);
        let su = game("ELDEN RING [v1.12] Free Download", "https://b.test", None, SourceLabel::Steamunlocked);
        let matched = match_sources(vec![sr], vec![su]);
        assert_eq!(matched.len(), 1);
    }

    #[test]
    fn keeps_unmatched_as_single_source() {
        let sr = game("Only On Steamrip", "https://a.test", None, SourceLabel::Steamrip);
        let su = game("Only On Steamunlocked", "https://b.test", None, SourceLabel::Steamunlocked);
        let matched = match_sources(vec![sr], vec![su]);
        assert_eq!(matched.len(), 2);
    }

    #[test]
    fn bucketed_matching_matches_across_buckets() {
        // Different first letters -> different buckets; unpaired entries survive.
        let a = game("Alpha Game", "https://a.test", None, SourceLabel::Steamrip);
        let b = game("Beta Game", "https://b.test", None, SourceLabel::Steamunlocked);
        let matched = match_sources_bucketed(vec![a], vec![b]);
        assert_eq!(matched.len(), 2);
    }

    #[test]
    fn bucketed_matching_pairs_same_title() {
        let a = game("Zombie Dawn Free Download", "https://a.test", None, SourceLabel::Steamrip);
        let b = game("Zombie Dawn (v1.2)", "https://b.test", Some("1.2"), SourceLabel::Steamunlocked);
        let matched = match_sources_bucketed(vec![a], vec![b]);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].candidates.len(), 2);
    }

    #[test]
    fn dedupes_same_normalized_title() {
        let a = game("Doom", "https://a.test", None, SourceLabel::Steamrip);
        let b = game("DOOM", "https://b.test", Some("3"), SourceLabel::Steamunlocked);
        let matched = match_sources(vec![a], vec![b]);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].best_download.version.as_deref(), Some("3"));
    }

    #[test]
    fn similarity_bounds() {
        assert!(title_similarity("Half-Life 2", "Half-Life 2") > 0.99);
        assert!(title_similarity("Portal", "Portal 2") >= 0.5);
        assert!(title_similarity("Portal", "Half-Life") < 0.85);
    }
}
