use anyhow::Result;
use std::collections::{BTreeSet, HashMap};
use tracing::{info, warn};

use crate::config::{Config, RotationMode};
use crate::filter::Filters;
use crate::source::{Source, SourceRepo};
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

/// The actions a reconcile pass should take, computed purely from the current
/// source/target/state snapshot so the core logic can be unit-tested in isolation.
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    /// Managed repos to blacklist (user converted them to regular repos, or deleted them).
    blacklist: Vec<String>,
    /// Source repos with no counterpart on the target → create a new mirror.
    create: Vec<String>,
    /// Existing mirrors not yet tracked → start managing them.
    adopt: Vec<String>,
    /// Names present on the target as non-mirrors and not managed by us → leave alone.
    conflict: Vec<String>,
}

/// Pure planning core. `target_mirror` maps every repo name present on the target
/// to whether it is currently a mirror.
fn plan_actions(
    managed: &BTreeSet<String>,
    blacklist: &BTreeSet<String>,
    target_mirror: &HashMap<String, bool>,
    source: &[String],
) -> Plan {
    let mut plan = Plan::default();

    // Managed repos the user broke (converted) or removed (deleted) → blacklist.
    for name in managed {
        match target_mirror.get(name) {
            Some(true) => {}                                  // healthy mirror, leave it
            Some(false) => plan.blacklist.push(name.clone()), // converted to a regular repo
            None => plan.blacklist.push(name.clone()),        // deleted entirely
        }
    }
    let newly_blacklisted: BTreeSet<&String> = plan.blacklist.iter().collect();

    // Source repos → create / adopt / conflict.
    for name in source {
        if blacklist.contains(name) || newly_blacklisted.contains(name) {
            continue;
        }
        match target_mirror.get(name) {
            None => plan.create.push(name.clone()),
            Some(true) => {
                if !managed.contains(name) {
                    plan.adopt.push(name.clone());
                }
            }
            Some(false) => plan.conflict.push(name.clone()),
        }
    }

    plan.blacklist.sort();
    plan.create.sort();
    plan.adopt.sort();
    plan.conflict.sort();
    plan
}

impl Engine<'_> {
    /// One full pass. On `startup` it also runs the token-rotation check.
    pub async fn reconcile(&self, state: &mut State, startup: bool) -> Result<()> {
        info!(startup, "starting reconcile");

        // Snapshot the target.
        let target_repos = self.target.list_repos(self.caps.owner_is_org).await?;
        let target_by_name: HashMap<&str, &Repo> =
            target_repos.iter().map(|r| (r.name.as_str(), r)).collect();
        info!(
            "target has {} repos under '{}'",
            target_repos.len(),
            self.target.owner
        );

        // Token rotation (startup only).
        if startup {
            self.rotate_if_needed(state, &target_by_name).await?;
        }

        // Discover + filter the source.
        let source_repos = self.source.list_repos().await?;
        let total = source_repos.len();
        let selected = self.filters.apply(source_repos);
        info!(
            "source has {total} repos, {} selected after filters",
            selected.len()
        );

        // Compute the plan from the snapshot.
        let target_mirror: HashMap<String, bool> = target_repos
            .iter()
            .map(|r| (r.name.clone(), r.mirror))
            .collect();
        let source_names: Vec<String> = selected.iter().map(|r| r.name.clone()).collect();
        let source_by_name: HashMap<&str, &SourceRepo> =
            selected.iter().map(|r| (r.name.as_str(), r)).collect();
        let plan = plan_actions(
            &state.managed,
            &state.blacklist,
            &target_mirror,
            &source_names,
        );

        // 1. Blacklist broken mirrors.
        for name in &plan.blacklist {
            if target_by_name.contains_key(name.as_str()) {
                warn!("'{name}' was converted to a regular repo by the user → blacklisting");
            } else {
                warn!(
                    "managed mirror '{name}' is gone from the target (deleted by the user) \
                     → blacklisting (delete it from the state file to mirror it again)"
                );
            }
            state.blacklist.insert(name.clone());
            state.managed.remove(name);
        }

        // 2. Create missing mirrors.
        for name in &plan.create {
            let repo = source_by_name
                .get(name.as_str())
                .expect("a planned create always has a matching source repo");
            let private = self.cfg.mirror_private || repo.private;
            info!("creating mirror '{name}' (private={private})");
            self.target
                .migrate(
                    &repo.clone_url,
                    name,
                    self.source.service(),
                    &self.cfg.source_token,
                    &self.cfg.mirror_interval,
                    private,
                    repo.description.as_deref(),
                )
                .await?;
            state.managed.insert(name.clone());
        }

        // 3. Adopt pre-existing mirrors.
        for name in &plan.adopt {
            info!("adopting existing mirror '{name}'");
            state.managed.insert(name.clone());
        }

        // 4. Report conflicts.
        for name in &plan.conflict {
            warn!("'{name}' exists on the target but is not a mirror — skipping (not managed)");
        }

        // 5. Optionally force an immediate sync of everything we manage that is in source.
        if self.cfg.trigger_sync {
            for name in &source_names {
                if state.managed.contains(name) {
                    if let Err(e) = self.target.trigger_sync(name).await {
                        warn!("could not trigger sync for '{name}': {e:#}");
                    }
                }
            }
        }

        state.save()?;
        info!(
            "reconcile done: {} created, {} adopted, {} skipped | {} managed, {} blacklisted",
            plan.create.len(),
            plan.adopt.len(),
            plan.conflict.len(),
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
                if let Err(e) = self
                    .target
                    .patch_mirror_token(&name, &self.cfg.source_token)
                    .await
                {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bset(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }
    fn tmap(items: &[(&str, bool)]) -> HashMap<String, bool> {
        items.iter().map(|(n, m)| (n.to_string(), *m)).collect()
    }
    fn vnames(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn creates_new_source_repos() {
        let plan = plan_actions(&bset(&[]), &bset(&[]), &tmap(&[]), &vnames(&["a", "b"]));
        assert_eq!(plan.create, vnames(&["a", "b"]));
        assert!(plan.adopt.is_empty() && plan.blacklist.is_empty() && plan.conflict.is_empty());
    }

    #[test]
    fn adopts_untracked_existing_mirror() {
        let plan = plan_actions(
            &bset(&[]),
            &bset(&[]),
            &tmap(&[("a", true)]),
            &vnames(&["a"]),
        );
        assert_eq!(plan.adopt, vnames(&["a"]));
        assert!(plan.create.is_empty());
    }

    #[test]
    fn managed_healthy_mirror_is_a_noop() {
        let plan = plan_actions(
            &bset(&["a"]),
            &bset(&[]),
            &tmap(&[("a", true)]),
            &vnames(&["a"]),
        );
        assert_eq!(plan, Plan::default());
    }

    #[test]
    fn converted_mirror_is_blacklisted_not_recreated() {
        let plan = plan_actions(
            &bset(&["a"]),
            &bset(&[]),
            &tmap(&[("a", false)]),
            &vnames(&["a"]),
        );
        assert_eq!(plan.blacklist, vnames(&["a"]));
        assert!(plan.create.is_empty() && plan.conflict.is_empty() && plan.adopt.is_empty());
    }

    #[test]
    fn deleted_mirror_is_blacklisted_not_recreated() {
        let plan = plan_actions(&bset(&["a"]), &bset(&[]), &tmap(&[]), &vnames(&["a"]));
        assert_eq!(plan.blacklist, vnames(&["a"]));
        assert!(plan.create.is_empty());
    }

    #[test]
    fn blacklisted_source_is_skipped() {
        let plan = plan_actions(&bset(&[]), &bset(&["a"]), &tmap(&[]), &vnames(&["a", "b"]));
        assert_eq!(plan.create, vnames(&["b"]));
    }

    #[test]
    fn non_mirror_name_collision_is_a_conflict() {
        let plan = plan_actions(
            &bset(&[]),
            &bset(&[]),
            &tmap(&[("a", false)]),
            &vnames(&["a"]),
        );
        assert_eq!(plan.conflict, vnames(&["a"]));
        assert!(plan.create.is_empty() && plan.blacklist.is_empty());
    }

    #[test]
    fn mixed_scenario() {
        // a: managed+healthy (noop)   b: new (create)        c: untracked mirror (adopt)
        // d: managed+converted (blacklist, skip in source)   e: blacklisted (skip)
        // f: non-mirror collision (conflict)
        let plan = plan_actions(
            &bset(&["a", "d"]),
            &bset(&["e"]),
            &tmap(&[("a", true), ("c", true), ("d", false), ("f", false)]),
            &vnames(&["a", "b", "c", "d", "e", "f"]),
        );
        assert_eq!(plan.create, vnames(&["b"]));
        assert_eq!(plan.adopt, vnames(&["c"]));
        assert_eq!(plan.blacklist, vnames(&["d"]));
        assert_eq!(plan.conflict, vnames(&["f"]));
    }
}
