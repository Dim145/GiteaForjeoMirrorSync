use anyhow::Result;

use super::{encode_path, jbool, jstr, Source, SourceRepo};
use crate::config::OwnerType;
use crate::http::paginate;

/// List projects from GitLab (gitlab.com or self-hosted).
///
/// - Group: `GET /groups/{id}/projects?include_subgroups=true&with_shared=false`.
/// - User:  `GET /users/{id}/projects`.
pub async fn list(src: &Source, ot: OwnerType) -> Result<Vec<SourceRepo>> {
    let base = src.base_url.trim_end_matches('/'); // already ends with /api/v4
    let size = 100usize;
    let owner = encode_path(&src.owner);
    let items = match ot {
        OwnerType::Org => {
            let url = format!("{base}/groups/{owner}/projects");
            paginate(
                |p| {
                    src.auth(src.http.get(&url)).query(&[
                        ("per_page", size.to_string()),
                        ("page", p.to_string()),
                        ("include_subgroups", "true".to_string()),
                        ("with_shared", "false".to_string()),
                    ])
                },
                size,
                200,
            )
            .await?
        }
        _ => {
            let url = format!("{base}/users/{owner}/projects");
            paginate(
                |p| {
                    src.auth(src.http.get(&url))
                        .query(&[("per_page", size.to_string()), ("page", p.to_string())])
                },
                size,
                200,
            )
            .await?
        }
    };
    Ok(items.iter().filter_map(map_repo).collect())
}

fn map_repo(v: &serde_json::Value) -> Option<SourceRepo> {
    Some(SourceRepo {
        name: jstr(v, "path")?, // repo slug, not the display "name"
        clone_url: jstr(v, "http_url_to_repo")?,
        private: v
            .get("visibility")
            .and_then(|x| x.as_str())
            .map(|s| s != "public")
            .unwrap_or(true),
        fork: v
            .get("forked_from_project")
            .map(|x| !x.is_null())
            .unwrap_or(false),
        archived: jbool(v, "archived"),
        description: jstr(v, "description"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_gitlab_project_uses_path() {
        let v = json!({
            "path": "my-proj",
            "name": "My Proj",
            "http_url_to_repo": "https://gitlab.com/g/my-proj.git",
            "visibility": "private",
            "archived": true,
            "forked_from_project": {"id": 1},
            "description": "d"
        });
        let r = map_repo(&v).unwrap();
        assert_eq!(r.name, "my-proj"); // path, not display name
        assert_eq!(r.clone_url, "https://gitlab.com/g/my-proj.git");
        assert!(r.private);
        assert!(r.fork);
        assert!(r.archived);
    }

    #[test]
    fn public_project_not_private_not_fork() {
        let v = json!({"path": "p", "http_url_to_repo": "u", "visibility": "public"});
        let r = map_repo(&v).unwrap();
        assert!(!r.private);
        assert!(!r.fork);
        assert!(!r.archived);
    }

    #[test]
    fn missing_visibility_defaults_private() {
        let v = json!({"path": "p", "http_url_to_repo": "u"});
        assert!(map_repo(&v).unwrap().private);
    }
}
