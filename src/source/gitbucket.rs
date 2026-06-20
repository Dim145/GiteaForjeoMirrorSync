use anyhow::Result;

use super::{jbool, jstr, Source, SourceRepo};
use crate::config::OwnerType;
use crate::http::paginate;

/// List repos from GitBucket (GitHub-compatible API under `/api/v3`).
///
/// GitBucket only implements a subset of the GitHub API and its pagination
/// support varies by version; the de-dup in `Source::list_repos` guards against
/// a server that ignores `page`/`per_page`.
pub async fn list(src: &Source, ot: OwnerType) -> Result<Vec<SourceRepo>> {
    let base = src.base_url.trim_end_matches('/'); // already ends with /api/v3
    let size = 100usize;
    let items = match ot {
        OwnerType::Org => {
            let url = format!("{base}/orgs/{}/repos", src.owner);
            paginate(
                |p| {
                    src.auth(src.http.get(&url))
                        .query(&[("per_page", size.to_string()), ("page", p.to_string())])
                },
                size,
                100,
            )
            .await?
        }
        _ => {
            let url = format!("{base}/user/repos");
            let all = paginate(
                |p| {
                    src.auth(src.http.get(&url))
                        .query(&[("per_page", size.to_string()), ("page", p.to_string())])
                },
                size,
                100,
            )
            .await?;
            let owned: Vec<_> = all
                .into_iter()
                .filter(|v| owner_matches(v, &src.owner))
                .collect();
            if !owned.is_empty() {
                owned
            } else {
                let url2 = format!("{base}/users/{}/repos", src.owner);
                paginate(
                    |p| {
                        src.auth(src.http.get(&url2))
                            .query(&[("per_page", size.to_string()), ("page", p.to_string())])
                    },
                    size,
                    100,
                )
                .await?
            }
        }
    };
    Ok(items.iter().filter_map(map_repo).collect())
}

fn owner_matches(v: &serde_json::Value, owner: &str) -> bool {
    v.get("owner")
        .and_then(|o| o.get("login"))
        .and_then(|l| l.as_str())
        .map(|l| l.eq_ignore_ascii_case(owner))
        .unwrap_or(false)
}

fn map_repo(v: &serde_json::Value) -> Option<SourceRepo> {
    Some(SourceRepo {
        name: jstr(v, "name")?,
        clone_url: jstr(v, "clone_url")?,
        private: jbool(v, "private"),
        fork: jbool(v, "fork"),
        archived: jbool(v, "archived"),
        description: jstr(v, "description"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_gitbucket_repo() {
        let v = json!({
            "name": "r",
            "clone_url": "http://gb.example/git/u/r.git",
            "private": true
        });
        let r = map_repo(&v).unwrap();
        assert_eq!(r.name, "r");
        assert_eq!(r.clone_url, "http://gb.example/git/u/r.git");
        assert!(r.private);
        assert!(!r.fork);
        assert!(!r.archived);
    }

    #[test]
    fn owner_matches_login() {
        assert!(owner_matches(&json!({"owner": {"login": "Acme"}}), "acme"));
        assert!(!owner_matches(&json!({"owner": {"login": "Acme"}}), "nope"));
    }
}
