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

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> SourceRepo {
        SourceRepo {
            name: name.into(),
            clone_url: format!("https://example.com/{name}.git"),
            private: false,
            fork: false,
            archived: false,
            description: None,
        }
    }

    fn base() -> Filters {
        Filters {
            include: None,
            exclude: None,
            limit: None,
            skip_forks: false,
            skip_archived: false,
            include_private: true,
        }
    }

    fn names(repos: &[SourceRepo]) -> Vec<&str> {
        repos.iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn passthrough_sorts_by_name() {
        let f = base();
        let out = f.apply(vec![repo("charlie"), repo("alpha"), repo("bravo")]);
        assert_eq!(names(&out), ["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn include_regex_keeps_only_matches() {
        let mut f = base();
        f.include = Some(Regex::new("^app-").unwrap());
        let out = f.apply(vec![repo("app-web"), repo("lib-core"), repo("app-api")]);
        assert_eq!(names(&out), ["app-api", "app-web"]);
    }

    #[test]
    fn exclude_regex_drops_matches() {
        let mut f = base();
        f.exclude = Some(Regex::new("-archive$").unwrap());
        let out = f.apply(vec![repo("a"), repo("b-archive"), repo("c")]);
        assert_eq!(names(&out), ["a", "c"]);
    }

    #[test]
    fn skip_flags_and_private() {
        let mut f = base();
        f.skip_forks = true;
        f.skip_archived = true;
        f.include_private = false;

        let mut a_fork = repo("a-fork");
        a_fork.fork = true;
        let mut b_arch = repo("b-arch");
        b_arch.archived = true;
        let mut c_priv = repo("c-priv");
        c_priv.private = true;

        let out = f.apply(vec![a_fork, b_arch, c_priv, repo("keep")]);
        assert_eq!(names(&out), ["keep"]);
    }

    #[test]
    fn limit_applies_after_sort() {
        let mut f = base();
        f.limit = Some(2);
        let out = f.apply(vec![repo("d"), repo("a"), repo("c"), repo("b")]);
        assert_eq!(names(&out), ["a", "b"]);
    }
}
