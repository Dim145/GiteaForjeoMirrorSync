use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Persistent state, stored as a small JSON file written atomically.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    /// Mirrors created/adopted and currently managed by this tool.
    #[serde(default)]
    pub managed: BTreeSet<String>,
    /// Repos the user intentionally un-mirrored or deleted; never re-created.
    /// Remove an entry here by hand to let the tool mirror it again.
    #[serde(default)]
    pub blacklist: BTreeSet<String>,
    /// SHA-256 of the source token last seen, to detect rotation at startup.
    #[serde(default)]
    pub token_fingerprint: Option<String>,

    #[serde(skip)]
    path: PathBuf,
}

impl State {
    pub fn load(path: &Path) -> Result<Self> {
        if path.exists() {
            let data = std::fs::read_to_string(path)
                .with_context(|| format!("reading state file {}", path.display()))?;
            let mut s: State = serde_json::from_str(&data)
                .with_context(|| format!("parsing state file {}", path.display()))?;
            s.path = path.to_path_buf();
            Ok(s)
        } else {
            Ok(State {
                path: path.to_path_buf(),
                ..Default::default()
            })
        }
    }

    /// Write the state atomically (temp file + rename) so a crash can't corrupt it.
    pub fn save(&self) -> Result<()> {
        let file_name = self
            .path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("invalid state path: {}", self.path.display()))?;
        let tmp = self
            .path
            .with_file_name(format!("{}.tmp", file_name.to_string_lossy()));
        let data = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, data).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming {} -> {}", tmp.display(), self.path.display()))?;
        Ok(())
    }
}

/// Stable fingerprint of a secret (we never persist the secret itself).
pub fn fingerprint(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_and_distinct() {
        assert_eq!(fingerprint("abc"), fingerprint("abc"));
        assert_ne!(fingerprint("abc"), fingerprint("abd"));
        assert_eq!(fingerprint("abc").len(), 64); // sha-256 hex
    }

    #[test]
    fn load_missing_file_is_empty() {
        let path = std::env::temp_dir().join("gms-test-missing-xyz.json");
        let _ = std::fs::remove_file(&path);
        let s = State::load(&path).unwrap();
        assert!(s.managed.is_empty());
        assert!(s.blacklist.is_empty());
        assert!(s.token_fingerprint.is_none());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let path = std::env::temp_dir().join("gms-test-roundtrip.json");
        let _ = std::fs::remove_file(&path);

        let mut s = State::load(&path).unwrap();
        s.managed.insert("repo-a".into());
        s.blacklist.insert("repo-b".into());
        s.token_fingerprint = Some("deadbeef".into());
        s.save().unwrap();

        let loaded = State::load(&path).unwrap();
        assert!(loaded.managed.contains("repo-a"));
        assert!(loaded.blacklist.contains("repo-b"));
        assert_eq!(loaded.token_fingerprint.as_deref(), Some("deadbeef"));

        let _ = std::fs::remove_file(&path);
    }
}
