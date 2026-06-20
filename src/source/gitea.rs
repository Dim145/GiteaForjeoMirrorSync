use anyhow::Result;

use super::{jbool, jstr, Source, SourceRepo};
use crate::config::OwnerType;
use crate::http::paginate;

/// List repos from a Gitea/Forgejo source.
///
/// - Org:  `GET /orgs/{org}/repos`.
/// - User: `GET /user/repos` (authenticated, incl. private) filtered to the owner;
///   falls back to that user's public repos for a different user.
pub async fn list(src: &Source, ot: OwnerType) -> Result<Vec<SourceRepo>> {
    let base = src.base_url.trim_end_matches('/'); // already ends with /api/v1
    let size = 50usize; // Gitea caps page size (MAX_RESPONSE_ITEMS, default 50)
    let items = match ot {
        OwnerType::Org => {
            let url = format!("{base}/orgs/{}/repos", src.owner);
            paginate(
                |p| {
                    src.auth(src.http.get(&url))
                        .query(&[("limit", size.to_string()), ("page", p.to_string())])
                },
                size,
                500,
            )
            .await?
        }
        _ => {
            let url = format!("{base}/user/repos");
            let all = paginate(
                |p| {
                    src.auth(src.http.get(&url))
                        .query(&[("limit", size.to_string()), ("page", p.to_string())])
                },
                size,
                500,
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
                            .query(&[("limit", size.to_string()), ("page", p.to_string())])
                    },
                    size,
                    500,
                )
                .await?
            }
        }
    };
    Ok(items.iter().filter_map(map_repo).collect())
}

fn owner_matches(v: &serde_json::Value, owner: &str) -> bool {
    v.get("owner")
        .and_then(|o| o.get("login").or_else(|| o.get("username")))
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
    fn maps_gitea_repo() {
        let v = json!({
            "name": "r",
            "clone_url": "https://gitea.example/r.git",
            "private": false,
            "fork": true,
            "archived": false
        });
        let r = map_repo(&v).unwrap();
        assert_eq!(r.name, "r");
        assert_eq!(r.clone_url, "https://gitea.example/r.git");
        assert!(r.fork);
        assert!(!r.private);
    }

    #[test]
    fn owner_matches_login_or_username() {
        assert!(owner_matches(&json!({"owner": {"login": "Org"}}), "org"));
        assert!(owner_matches(&json!({"owner": {"username": "Bob"}}), "bob"));
        assert!(!owner_matches(
            &json!({"owner": {"username": "bob"}}),
            "alice"
        ));
    }
}
