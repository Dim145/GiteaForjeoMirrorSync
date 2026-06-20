use anyhow::Result;

use super::{jbool, jstr, Source, SourceRepo};
use crate::config::OwnerType;
use crate::http::paginate;

/// List repos from GitHub (or GitHub Enterprise via `GMS_SOURCE_URL`).
///
/// - Org: `GET /orgs/{org}/repos?type=all` (includes private if the token can see them).
/// - User: `GET /user/repos` (the token owner's own repos, incl. private), filtered
///   to the owner; falls back to that user's public repos for a different user.
pub async fn list(src: &Source, ot: OwnerType) -> Result<Vec<SourceRepo>> {
    let base = src.base_url.trim_end_matches('/');
    let size = 100usize;
    let items = match ot {
        OwnerType::Org => {
            let url = format!("{base}/orgs/{}/repos", src.owner);
            paginate(
                |p| {
                    src.auth(src.http.get(&url)).query(&[
                        ("per_page", size.to_string()),
                        ("page", p.to_string()),
                        ("type", "all".to_string()),
                    ])
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
                    src.auth(src.http.get(&url)).query(&[
                        ("per_page", size.to_string()),
                        ("page", p.to_string()),
                        ("affiliation", "owner".to_string()),
                        ("visibility", "all".to_string()),
                    ])
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
                // owner is not the authenticated account → list that user's public repos
                let url2 = format!("{base}/users/{}/repos", src.owner);
                paginate(
                    |p| {
                        src.auth(src.http.get(&url2)).query(&[
                            ("per_page", size.to_string()),
                            ("page", p.to_string()),
                            ("type", "all".to_string()),
                        ])
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
    fn maps_github_repo() {
        let v = json!({
            "name": "hello",
            "clone_url": "https://github.com/me/hello.git",
            "private": true,
            "fork": false,
            "archived": true,
            "description": "hi"
        });
        let r = map_repo(&v).unwrap();
        assert_eq!(r.name, "hello");
        assert_eq!(r.clone_url, "https://github.com/me/hello.git");
        assert!(r.private);
        assert!(!r.fork);
        assert!(r.archived);
        assert_eq!(r.description.as_deref(), Some("hi"));
    }

    #[test]
    fn map_repo_requires_name_and_clone_url() {
        assert!(map_repo(&json!({"name": "x"})).is_none());
        assert!(map_repo(&json!({"clone_url": "u"})).is_none());
    }

    #[test]
    fn owner_matches_is_case_insensitive() {
        assert!(owner_matches(
            &json!({"owner": {"login": "MyOrg"}}),
            "myorg"
        ));
        assert!(!owner_matches(
            &json!({"owner": {"login": "MyOrg"}}),
            "other"
        ));
        assert!(!owner_matches(&json!({}), "x"));
    }
}
