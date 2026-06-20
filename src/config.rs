use anyhow::{Context, Result};
use figment::{providers::Env, Figment};
use serde::Deserialize;
use std::path::PathBuf;

/// Which kind of forge we mirror *from*.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SourceType {
    Github,
    Gitlab,
    Gitea,
    Gitbucket,
}

/// Whether an owner is an individual user or an organization/group.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum OwnerType {
    /// Detect automatically by probing the org/group endpoint.
    #[default]
    Auto,
    User,
    Org,
}

/// What to do when the source token changed and the target instance has no
/// API to re-authenticate an existing pull mirror.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum RotationMode {
    /// PATCH mirror_token if supported (Gitea >= 1.27), otherwise delete + recreate.
    #[default]
    Auto,
    /// Always delete + recreate the mirror with the new token.
    Recreate,
    /// Do nothing but log the repos that need a manual re-auth.
    Warn,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    // ---- Target (the local Gitea/Forgejo instance that holds the mirrors) ----
    pub target_url: String,
    pub target_token: String,
    pub target_owner: String,
    #[serde(default)]
    pub target_owner_type: OwnerType,

    // ---- Source (the forge we mirror from) ----
    pub source_type: SourceType,
    #[serde(default)]
    pub source_url: Option<String>,
    pub source_token: String,
    pub source_owner: String,
    #[serde(default)]
    pub source_owner_type: OwnerType,

    // ---- Scheduling (discovery cadence; the actual git sync is done by Gitea) ----
    #[serde(default = "d_cron")]
    pub cron: String,

    // ---- Filters ----
    #[serde(default)]
    pub filter_include: Option<String>,
    #[serde(default)]
    pub filter_exclude: Option<String>,
    #[serde(default)]
    pub filter_limit: Option<usize>,
    #[serde(default)]
    pub skip_forks: bool,
    #[serde(default)]
    pub skip_archived: bool,
    #[serde(default = "d_true")]
    pub include_private: bool,

    // ---- Mirror behavior ----
    #[serde(default = "d_interval")]
    pub mirror_interval: String,
    #[serde(default = "d_true")]
    pub mirror_private: bool,
    #[serde(default)]
    pub trigger_sync: bool,
    #[serde(default)]
    pub rotation_mode: RotationMode,

    // ---- State ----
    #[serde(default = "d_state")]
    pub state_file: PathBuf,
}

fn d_cron() -> String {
    // 6-field cron (sec min hour day-of-month month day-of-week): hourly at :00.
    "0 0 * * * *".into()
}
fn d_interval() -> String {
    "8h0m0s".into()
}
fn d_true() -> bool {
    true
}
fn d_state() -> PathBuf {
    PathBuf::from("gms-state.json")
}

impl Config {
    /// Load configuration from `GMS_*` environment variables.
    pub fn load() -> Result<Self> {
        Figment::new()
            .merge(Env::prefixed("GMS_"))
            .extract()
            .context("loading configuration from GMS_* environment variables")
    }

    /// Compute the API base URL for the configured source forge, applying the
    /// per-forge suffix (e.g. `/api/v4` for GitLab) when the user gave a bare host.
    pub fn source_api_base(&self) -> Result<String> {
        let raw = match &self.source_url {
            Some(u) => u.trim_end_matches('/').to_string(),
            None => match self.source_type {
                SourceType::Github => "https://api.github.com".into(),
                SourceType::Gitlab => "https://gitlab.com".into(),
                other => anyhow::bail!("GMS_SOURCE_URL is required for source type {other:?}"),
            },
        };
        Ok(match self.source_type {
            SourceType::Github => raw,
            SourceType::Gitlab => {
                if raw.contains("/api/v") {
                    raw
                } else {
                    format!("{raw}/api/v4")
                }
            }
            SourceType::Gitea => {
                if raw.ends_with("/api/v1") {
                    raw
                } else {
                    format!("{raw}/api/v1")
                }
            }
            SourceType::Gitbucket => {
                if raw.contains("/api/v3") {
                    raw
                } else {
                    format!("{raw}/api/v3")
                }
            }
        })
    }
}
