use anyhow::Result;
use std::collections::HashMap;
use tracing::{info, warn};

use crate::config::{Config, RotationMode};
use crate::filter::Filters;
use crate::source::Source;
use crate::state::{fingerprint, State};
use crate::target::{Capabilities, Repo, Target};

/// The reconciliation engine. Holds borrowed dependencies for the run loop.
pub struct Engine<'a> {
    pub cfg: &'a Config,
    pub target: &'a Target,
    pub source: &'a Source,
    pub filters: &'a Filters,
    pub caps: &'a Capabilities,
}

impl Engine<'_> {
    /// One full pass. On `startup` it also runs the token-rotation check.
    pub async fn reconcile(&self, state: &mut State, startup: bool) -> Result<()> {
        info!(startup, "starting reconcile");

        // 1. Snapshot the target.
        let target_repos = self.target.list_repos(self.caps.owner_is_org).await?;
        let target_map: HashMap<&str, &Repo> =
            target_repos.iter().map(|r| (r.name.as_str(), r)).collect();
        info!("target has {} repos under '{}'", target_repos.len(), self.target.owner);

        // 2. Token rotation (startup only).
        if startup {
            self.rotate_if_needed(state, &target_map).await?;
        }

        // 3. Detect mirrors the user broke (converted) or removed → blacklist them.
        for name in state.managed.iter().cloned().collect::<Vec<_>>() {
            match target_map.get(name.as_str()) {
                Some(r) if !r.mirror => {
                    warn!("'{name}' was converted to a regular repo by the user → blacklisting");
                    state.blacklist.insert(name.clone());
                    state.managed.remove(&name);
                }
                None => {
                    warn!(
                        "managed mirror '{name}' is gone from the target (deleted by the user) \
                         → blacklisting (delete it from the state file to mirror it again)"
                    );
                    state.blacklist.insert(name.clone());
                    state.managed.remove(&name);
                }
                Some(_) => {} // healthy mirror, leave it (Gitea keeps it in sync)
            }
        }

        // 4. Discover source repos and apply filters.
        let source_repos = self.source.list_repos().await?;
        let total = source_repos.len();
        let selected = self.filters.apply(source_repos);
        info!("source has {total} repos, {} selected after filters", selected.len());

        // 5. Create missing mirrors / adopt pre-existing ones.
        let (mut created, mut adopted, mut skipped) = (0usize, 0usize, 0usize);
        for repo in &selected {
            if state.blacklist.contains(&repo.name) {
                continue;
            }
            match target_map.get(repo.name.as_str()) {
                None => {
                    let private = self.cfg.mirror_private || repo.private;
                    info!("creating mirror '{}' (private={private})", repo.name);
                    self.target
                        .migrate(
                            &repo.clone_url,
                            &repo.name,
                            self.source.service(),
                            &self.cfg.source_token,
                            &self.cfg.mirror_interval,
                            private,
                            repo.description.as_deref(),
                        )
                        .await?;
                    state.managed.insert(repo.name.clone());
                    created += 1;
                }
                Some(r) if r.mirror => {
                    if state.managed.insert(repo.name.clone()) {
                        info!("adopting existing mirror '{}'", repo.name);
                        adopted += 1;
                    }
                    if self.cfg.trigger_sync {
                        if let Err(e) = self.target.trigger_sync(&repo.name).await {
                            warn!("could not trigger sync for '{}': {e:#}", repo.name);
                        }
                    }
                }
                Some(_) => {
                    warn!(
                        "'{}' exists on the target but is not a mirror — skipping (not managed)",
                        repo.name
                    );
                    skipped += 1;
                }
            }
        }

        state.save()?;
        info!(
            "reconcile done: {created} created, {adopted} adopted, {skipped} skipped \
             | {} managed, {} blacklisted",
            state.managed.len(),
            state.blacklist.len()
        );
        Ok(())
    }

    /// If the source token changed since last run, re-authenticate every managed
    /// mirror using the configured rotation strategy.
    async fn rotate_if_needed(
        &self,
        state: &mut State,
        target_map: &HashMap<&str, &Repo>,
    ) -> Result<()> {
        let fp = fingerprint(&self.cfg.source_token);
        match &state.token_fingerprint {
            Some(old) if old == &fp => {
                info!("source token unchanged");
                return Ok(());
            }
            None => {
                info!("first run: recording source token fingerprint");
                state.token_fingerprint = Some(fp);
                state.save()?;
                return Ok(());
            }
            Some(_) => {}
        }

        let names: Vec<String> = state.managed.iter().cloned().collect();
        info!(
            "source token changed → re-authenticating {} managed mirror(s) (mode={:?})",
            names.len(),
            self.cfg.rotation_mode
        );
        for name in names {
            let Some(&repo) = target_map.get(name.as_str()) else {
                continue;
            };
            if !repo.mirror {
                continue; // handled by the broken-mirror detection
            }
            let use_patch = match self.cfg.rotation_mode {
                RotationMode::Warn => {
                    warn!("re-auth needed for '{name}' (rotation_mode=warn) — update it manually");
                    continue;
                }
                RotationMode::Recreate => false,
                RotationMode::Auto => self.caps.supports_pull_mirror_patch,
            };
            if use_patch {
                info!("rotating token for '{name}' via PATCH mirror_token");
                if let Err(e) = self.target.patch_mirror_token(&name, &self.cfg.source_token).await {
                    warn!("PATCH failed for '{name}': {e:#} — falling back to recreate");
                    self.recreate(&name, repo).await?;
                }
            } else {
                self.recreate(&name, repo).await?;
            }
        }
        state.token_fingerprint = Some(fp);
        state.save()?;
        Ok(())
    }

    /// Re-authenticate a mirror by deleting it and migrating it again with the
    /// current token, reusing the source URL the target still records.
    async fn recreate(&self, name: &str, repo: &Repo) -> Result<()> {
        if repo.original_url.is_empty() {
            warn!("cannot recreate '{name}': target has no original_url; skipping re-auth");
            return Ok(());
        }
        info!("re-authenticating '{name}' via delete + re-migrate");
        self.target.delete(name).await?;
        self.target
            .migrate(
                &repo.original_url,
                name,
                self.source.service(),
                &self.cfg.source_token,
                &self.cfg.mirror_interval,
                repo.private,
                None,
            )
            .await?;
        Ok(())
    }
}
