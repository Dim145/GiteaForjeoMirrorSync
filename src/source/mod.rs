mod gitbucket;
mod gitea;
mod github;
mod gitlab;

use anyhow::Result;
use reqwest::{Client, RequestBuilder};

use crate::config::{Config, OwnerType, SourceType};

/// A repository discovered on a source forge, normalized across forges.
#[derive(Debug, Clone)]
pub struct SourceRepo {
    pub name: String,
    pub clone_url: String,
    pub private: bool,
    pub fork: bool,
    pub archived: bool,
    pub description: Option<String>,
}

/// A configured source forge. Adapters live in the submodules and read the
/// (module-private) fields of this struct directly.
pub struct Source {
    kind: SourceType,
    http: Client,
    base_url: String,
    token: String,
    owner: String,
    owner_type: OwnerType,
}

impl Source {
    pub fn from_config(cfg: &Config, http: Client) -> Result<Self> {
        Ok(Self {
            kind: cfg.source_type,
            http,
            base_url: cfg.source_api_base()?,
            token: cfg.source_token.clone(),
            owner: cfg.source_owner.clone(),
            owner_type: cfg.source_owner_type,
        })
    }

    /// The Gitea/Forgejo `migrate` service value for this source.
    pub fn service(&self) -> &'static str {
        match self.kind {
            SourceType::Github => "github",
            SourceType::Gitlab => "gitlab",
            SourceType::Gitea => "gitea",
            SourceType::Gitbucket => "gitbucket",
        }
    }

    /// List every repository under the configured owner.
    pub async fn list_repos(&self) -> Result<Vec<SourceRepo>> {
        let ot = self.effective_owner_type().await;
        let mut repos = match self.kind {
            SourceType::Github => github::list(self, ot).await?,
            SourceType::Gitlab => gitlab::list(self, ot).await?,
            SourceType::Gitea => gitea::list(self, ot).await?,
            SourceType::Gitbucket => gitbucket::list(self, ot).await?,
        };
        // Defensive de-dup in case a forge ignores pagination params.
        repos.sort_by(|a, b| a.name.cmp(&b.name));
        repos.dedup_by(|a, b| a.name == b.name);
        Ok(repos)
    }

    /// Apply the per-forge authentication to a request.
    fn auth(&self, rb: RequestBuilder) -> RequestBuilder {
        match self.kind {
            SourceType::Github => rb
                .header("Authorization", format!("Bearer {}", self.token))
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28"),
            SourceType::Gitlab => rb.header("PRIVATE-TOKEN", self.token.clone()),
            SourceType::Gitea | SourceType::Gitbucket => {
                rb.header("Authorization", format!("token {}", self.token))
            }
        }
    }

    /// Resolve `Auto` owner type by probing the org/group endpoint.
    async fn effective_owner_type(&self) -> OwnerType {
        if self.owner_type != OwnerType::Auto {
            return self.owner_type;
        }
        let base = self.base_url.trim_end_matches('/');
        let url = match self.kind {
            SourceType::Gitlab => format!("{base}/groups/{}", encode_path(&self.owner)),
            _ => format!("{base}/orgs/{}", self.owner),
        };
        let is_org = matches!(self.auth(self.http.get(&url)).send().await, Ok(r) if r.status().is_success());
        let resolved = if is_org { OwnerType::Org } else { OwnerType::User };
        tracing::debug!(owner = %self.owner, ?resolved, "auto-detected source owner type");
        resolved
    }
}

/// Minimal percent-encoding for path segments (GitLab group paths contain `/`).
pub(crate) fn encode_path(s: &str) -> String {
    s.replace('/', "%2F").replace(' ', "%20")
}

pub(crate) fn jstr(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(String::from)
}

pub(crate) fn jbool(v: &serde_json::Value, key: &str) -> bool {
    v.get(key).and_then(|x| x.as_bool()).unwrap_or(false)
}
