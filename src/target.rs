use anyhow::{Context, Result};
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;

use crate::config::OwnerType;
use crate::http::{paginate, send_retry};

/// The local Gitea/Forgejo instance that holds the mirrors.
pub struct Target {
    http: Client,
    base: String, // scheme://host (no trailing slash)
    api: String,  // base + /api/v1
    token: String,
    pub owner: String,
}

/// Subset of the Gitea/Forgejo `Repository` object we care about.
#[derive(Debug, Deserialize)]
pub struct Repo {
    pub name: String,
    #[serde(default)]
    pub mirror: bool,
    #[serde(default)]
    pub original_url: String,
    #[serde(default)]
    pub private: bool,
}

/// What the target instance supports, detected at startup.
#[derive(Debug, Default, Clone)]
pub struct Capabilities {
    pub version: String,
    pub is_forgejo: bool,
    /// Whether `PATCH /repos/{owner}/{repo}` accepts `mirror_token` (Gitea >= 1.27).
    pub supports_pull_mirror_patch: bool,
    pub owner_is_org: bool,
}

impl Target {
    pub fn new(http: Client, url: &str, token: &str, owner: &str) -> Self {
        let base = url.trim_end_matches('/').to_string();
        let api = format!("{base}/api/v1");
        Self {
            http,
            base,
            api,
            token: token.to_string(),
            owner: owner.to_string(),
        }
    }

    fn auth(&self, rb: RequestBuilder) -> RequestBuilder {
        rb.header("Authorization", format!("token {}", self.token))
    }

    /// Probe the instance for version, flavor, mirror-PATCH support and owner type.
    /// Best-effort: any failed probe falls back to the safe default.
    pub async fn detect(&self, configured: OwnerType) -> Capabilities {
        let mut caps = Capabilities::default();

        if let Ok(resp) = self
            .auth(self.http.get(format!("{}/version", self.api)))
            .send()
            .await
        {
            if let Ok(v) = resp.json::<serde_json::Value>().await {
                caps.version = v
                    .get("version")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
            }
        }

        // Forgejo exposes a dedicated /api/forgejo/v1 namespace.
        caps.is_forgejo = matches!(
            self.http.get(format!("{}/api/forgejo/v1/version", self.base)).send().await,
            Ok(r) if r.status().is_success()
        );

        caps.supports_pull_mirror_patch = self.probe_patch_support().await;

        caps.owner_is_org = match configured {
            OwnerType::Org => true,
            OwnerType::User => false,
            OwnerType::Auto => matches!(
                self.auth(self.http.get(format!("{}/orgs/{}", self.api, self.owner))).send().await,
                Ok(r) if r.status().is_success()
            ),
        };

        caps
    }

    /// Detect whether `EditRepoOption` exposes `mirror_token` by reading swagger.
    async fn probe_patch_support(&self) -> bool {
        let url = format!("{}/swagger.v1.json", self.base);
        let Ok(resp) = self.http.get(&url).send().await else {
            return false;
        };
        if !resp.status().is_success() {
            return false;
        }
        let Ok(spec) = resp.json::<serde_json::Value>().await else {
            return false;
        };
        swagger_has_mirror_token(&spec)
    }

    /// List all repos under the configured owner.
    pub async fn list_repos(&self, owner_is_org: bool) -> Result<Vec<Repo>> {
        let path = if owner_is_org { "orgs" } else { "users" };
        let url = format!("{}/{}/{}/repos", self.api, path, self.owner);
        let size = 50usize;
        let items = paginate(
            |p| {
                self.auth(self.http.get(&url))
                    .query(&[("limit", size.to_string()), ("page", p.to_string())])
            },
            size,
            500,
        )
        .await?;
        let repos = items
            .iter()
            .filter_map(|v| serde_json::from_value::<Repo>(v.clone()).ok())
            .collect();
        Ok(repos)
    }

    /// Create a pull mirror via `POST /repos/migrate`.
    #[allow(clippy::too_many_arguments)]
    pub async fn migrate(
        &self,
        clone_addr: &str,
        name: &str,
        service: &str,
        token: &str,
        interval: &str,
        private: bool,
        description: Option<&str>,
    ) -> Result<()> {
        let body = serde_json::json!({
            "clone_addr": clone_addr,
            "repo_name": name,
            "repo_owner": self.owner,
            "service": service,
            "mirror": true,
            "mirror_interval": interval,
            "auth_token": token,
            "private": private,
            "description": description.unwrap_or(""),
            "wiki": false,
            "lfs": false,
        });
        let url = format!("{}/repos/migrate", self.api);
        send_retry(|| self.auth(self.http.post(&url)).json(&body))
            .await
            .with_context(|| format!("migrating '{name}'"))?;
        Ok(())
    }

    pub async fn delete(&self, name: &str) -> Result<()> {
        let url = format!("{}/repos/{}/{}", self.api, self.owner, name);
        send_retry(|| self.auth(self.http.delete(&url)))
            .await
            .with_context(|| format!("deleting '{name}'"))?;
        Ok(())
    }

    /// Update an existing pull mirror's source token (Gitea >= 1.27 only).
    pub async fn patch_mirror_token(&self, name: &str, token: &str) -> Result<()> {
        let url = format!("{}/repos/{}/{}", self.api, self.owner, name);
        let body = serde_json::json!({ "mirror_token": token });
        send_retry(|| self.auth(self.http.patch(&url)).json(&body))
            .await
            .with_context(|| format!("patching mirror token for '{name}'"))?;
        Ok(())
    }

    /// Trigger an immediate (async) mirror sync.
    pub async fn trigger_sync(&self, name: &str) -> Result<()> {
        let url = format!("{}/repos/{}/{}/mirror-sync", self.api, self.owner, name);
        send_retry(|| self.auth(self.http.post(&url)))
            .await
            .with_context(|| format!("triggering sync for '{name}'"))?;
        Ok(())
    }
}

/// Whether a swagger spec declares the `mirror_token` field on `EditRepoOption`.
fn swagger_has_mirror_token(spec: &serde_json::Value) -> bool {
    spec.get("definitions")
        .and_then(|d| d.get("EditRepoOption"))
        .and_then(|e| e.get("properties"))
        .and_then(|p| p.get("mirror_token"))
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn swagger_detects_mirror_token() {
        let with = json!({
            "definitions": { "EditRepoOption": { "properties": {
                "mirror_interval": {"type": "string"},
                "mirror_token": {"type": "string"}
            }}}
        });
        let without = json!({
            "definitions": { "EditRepoOption": { "properties": {
                "mirror_interval": {"type": "string"}
            }}}
        });
        assert!(swagger_has_mirror_token(&with));
        assert!(!swagger_has_mirror_token(&without));
        assert!(!swagger_has_mirror_token(&json!({})));
    }

    #[test]
    fn repo_deserializes_with_defaults() {
        // A converted/normal repo: only some fields present.
        let r: Repo = serde_json::from_value(json!({
            "name": "x",
            "original_url": "https://src/x.git"
        }))
        .unwrap();
        assert_eq!(r.name, "x");
        assert!(!r.mirror); // defaulted
        assert_eq!(r.original_url, "https://src/x.git");

        let m: Repo = serde_json::from_value(json!({
            "name": "y", "mirror": true, "private": true, "original_url": ""
        }))
        .unwrap();
        assert!(m.mirror);
        assert!(m.private);
    }
}
