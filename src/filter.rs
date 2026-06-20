use anyhow::{Context, Result};
use regex::Regex;

use crate::config::Config;
use crate::source::SourceRepo;

/// Compiled repo selection rules.
pub struct Filters {
    include: Option<Regex>,
    exclude: Option<Regex>,
    limit: Option<usize>,
    skip_forks: bool,
    skip_archived: bool,
    include_private: bool,
}

impl Filters {
    pub fn from_config(cfg: &Config) -> Result<Self> {
        Ok(Self {
            include: cfg
                .filter_include
                .as_deref()
                .map(Regex::new)
                .transpose()
                .context("compiling GMS_FILTER_INCLUDE")?,
            exclude: cfg
                .filter_exclude
                .as_deref()
                .map(Regex::new)
                .transpose()
                .context("compiling GMS_FILTER_EXCLUDE")?,
            limit: cfg.filter_limit,
            skip_forks: cfg.skip_forks,
            skip_archived: cfg.skip_archived,
            include_private: cfg.include_private,
        })
    }

    /// Apply the filters and return the selected repos, sorted by name. The
    /// quantity limit is applied last so it is deterministic.
    pub fn apply(&self, repos: Vec<SourceRepo>) -> Vec<SourceRepo> {
        let mut out: Vec<SourceRepo> = repos
            .into_iter()
            .filter(|r| {
                if !self.include_private && r.private {
                    return false;
                }
                if self.skip_forks && r.fork {
                    return false;
                }
                if self.skip_archived && r.archived {
                    return false;
                }
                if let Some(inc) = &self.include {
                    if !inc.is_match(&r.name) {
                        return false;
                    }
                }
                if let Some(exc) = &self.exclude {
                    if exc.is_match(&r.name) {
                        return false;
                    }
                }
                true
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        if let Some(lim) = self.limit {
            out.truncate(lim);
        }
        out
    }
}
