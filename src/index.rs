use crate::models::{IndexEntry, ManifestEntry, SourceLabel, StateFile};
use anyhow::Context;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::info;

pub struct Index {
    pub entries: HashMap<String, IndexEntry>,
    checkpointed: HashSet<String>,
    state_path: Option<std::path::PathBuf>,
}

impl Index {
    pub fn load(state_path: Option<std::path::PathBuf>) -> anyhow::Result<Self> {
        if let Some(path) = &state_path {
            if path.is_file() {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("reading state file {}", path.display()))?;
                let state: StateFile = serde_json::from_str(&text)
                    .with_context(|| format!("parsing state file {}", path.display()))?;
                info!(
                    "Loaded state: {} entries from {}",
                    state.index.len(),
                    path.display()
                );
                return Ok(Self {
                    entries: state.index,
                    checkpointed: state.checkpoint.into_iter().collect(),
                    state_path: Some(path.clone()),
                });
            }
        }

        Ok(Self {
            entries: HashMap::new(),
            checkpointed: HashSet::new(),
            state_path,
        })
    }

    /// Remember the last published version, source and title for a slug.
    pub fn remember(
        &mut self,
        slug: &str,
        title: Option<String>,
        version: Option<String>,
        source: Option<SourceLabel>,
    ) {
        let published_at = match self.entries.get(slug) {
            Some(existing) => existing.published_at.clone(),
            None => Some(now_rfc3339()),
        };
        self.entries.insert(
            slug.to_string(),
            IndexEntry {
                slug: slug.to_string(),
                title,
                last_version: version,
                last_download_source: source,
                published_at,
                verified: false,
            },
        );
    }

    pub fn checkpoint(&mut self, slug: &str) {
        self.checkpointed.insert(slug.to_string());
    }

    #[allow(dead_code)]
    pub fn has_been_checkpointed(&self, slug: &str) -> bool {
        self.checkpointed.contains(slug)
    }

    /// Whether the latest candidate differs from the last published record.
    pub fn has_changes(
        &self,
        slug: &str,
        version: Option<&str>,
        source: &SourceLabel,
    ) -> bool {
        match self.entries.get(slug) {
            Some(entry) => {
                let same_source = entry.last_download_source.as_ref() == Some(source);
                let same_version = entry.last_version.as_deref() == version;
                !(same_source && same_version)
            }
            None => true,
        }
    }

    /// Export the published list for the dedicated update-check workflow.
    pub fn manifest(&self) -> Vec<ManifestEntry> {
        let mut rows: Vec<ManifestEntry> = self
            .entries
            .values()
            .map(|e| ManifestEntry {
                slug: e.slug.clone(),
                title: e.title.clone(),
                version: e.last_version.clone(),
                source: e.last_download_source.as_ref().map(|s| s.to_string()),
                published_at: e.published_at.clone(),
            })
            .collect();
        rows.sort_by(|a, b| a.slug.cmp(&b.slug));
        rows
    }

    /// Atomic write: write a tmp file then rename over the target.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = match &self.state_path {
            Some(p) => p,
            None => return Ok(()),
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating state dir {}", parent.display()))?;
        }

        let state = StateFile {
            last_run_at: Some(now_rfc3339()),
            index: self.entries.clone(),
            checkpoint: self.checkpointed.iter().cloned().collect(),
        };
        let text = serde_json::to_string_pretty(&state)?;

        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &text)
            .with_context(|| format!("writing state tmp file {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("renaming state file into {}", path.display()))?;
        info!(
            "Wrote state file: {} ({} entries)",
            path.display(),
            self.entries.len()
        );
        Ok(())
    }
}

/// RFC3339-ish UTC timestamp without pulling chrono; seconds precision is
/// enough for run bookkeeping.
pub fn now_rfc3339() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let secs = now.as_secs();
    let days = secs / 86_400;
    let (y, mo, d) = civil_from_days(days as i64);
    let rem = secs % 86_400;
    format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days-since-epoch to (year, month, day) — Howard Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// Verified-flag plumbing: `verified` is only set by confirmations from the
// live API (create/patch responses, reconcile hits). Entries loaded from
// older state files default to false so they are republished.
impl Index {
    /// Record a listing after a confirmed create/patch on the site.
    pub fn remember_verified(
        &mut self,
        slug: &str,
        title: Option<String>,
        version: Option<String>,
        source: Option<SourceLabel>,
    ) {
        let published_at = match self.entries.get(slug) {
            Some(existing) => existing.published_at.clone(),
            None => Some(now_rfc3339()),
        };
        self.entries.insert(
            slug.to_string(),
            IndexEntry {
                slug: slug.to_string(),
                title,
                last_version: version,
                last_download_source: source,
                published_at,
                verified: true,
            },
        );
    }

    /// Mark an existing entry as confirmed on the site (reconcile).
    pub fn mark_verified(&mut self, slug: &str) {
        if let Some(entry) = self.entries.get_mut(slug) {
            entry.verified = true;
        }
    }

    /// Slugs present in state but never confirmed against the live site.
    pub fn unverified(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|(_, e)| !e.verified)
            .map(|(s, _)| s.clone())
            .collect()
    }
}

impl Index {
    /// Reconcile helper: mark verified if present, otherwise insert a new
    /// verified entry for a listing confirmed to exist on the site.
    pub fn verify_or_insert(&mut self, slug: &str, title: Option<String>) {
        if self.entries.contains_key(slug) {
            if let Some(entry) = self.entries.get_mut(slug) {
                entry.verified = true;
                if entry.title.is_none() {
                    entry.title = title.clone();
                }
            }
        } else {
            self.entries.insert(
                slug.to_string(),
                IndexEntry {
                    slug: slug.to_string(),
                    title,
                    last_version: None,
                    last_download_source: None,
                    published_at: Some(now_rfc3339()),
                    verified: true,
                },
            );
        }
    }
}

impl Index {
    /// Where the published-games manifest is written: beside the state file.
    pub fn manifest_path(&self) -> Option<std::path::PathBuf> {
        self.state_path
            .as_ref()
            .map(|p| p.with_file_name("published-games.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_and_detect_changes() {
        let mut index = Index::load(None).unwrap();
        assert!(index.has_changes("doom", None, &SourceLabel::Steamrip));

        index.remember(
            "doom",
            Some("DOOM".into()),
            Some("1.0".into()),
            Some(SourceLabel::Steamrip),
        );
        assert!(!index.has_changes("doom", Some("1.0"), &SourceLabel::Steamrip));
        assert!(index.has_changes("doom", Some("2.0"), &SourceLabel::Steamrip));
        assert!(index.has_changes(
            "doom",
            Some("1.0"),
            &SourceLabel::Steamunlocked
        ));
        assert_eq!(index.entries["doom"].title.as_deref(), Some("DOOM"));
        assert!(index.entries["doom"].published_at.is_some());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "autogamivoid-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");

        let mut index = Index::load(Some(path.clone())).unwrap();
        index.remember(
            "doom",
            Some("DOOM".into()),
            Some("1.0".into()),
            Some(SourceLabel::Steamrip),
        );
        index.checkpoint("doom");
        index.save().unwrap();

        let reloaded = Index::load(Some(path.clone())).unwrap();
        assert!(!reloaded.has_changes("doom", Some("1.0"), &SourceLabel::Steamrip));
        assert_eq!(reloaded.entries["doom"].title.as_deref(), Some("DOOM"));

        let manifest = reloaded.manifest();
        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest[0].slug, "doom");
        assert_eq!(manifest[0].title.as_deref(), Some("DOOM"));

        std::fs::remove_file(&path).ok();
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn timestamp_is_rfc3339_like() {
        let ts = now_rfc3339();
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
    }
}
