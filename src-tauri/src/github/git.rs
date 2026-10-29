use serde::{Deserialize, Serialize};

use super::config::RepoRef;
use super::http::{
    api_request, api_request_accept, api_url, next_link, send_json, send_raw, string_of, Method,
    Transport,
};
use super::state::blob_sha;
use super::GithubError;

const RAW_ACCEPT: &str = "application/vnd.github.raw+json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchRef {
    // refs/heads/を除いたもの
    pub name: String,
    pub commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeEntry {
    pub path: String,
    pub mode: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tree {
    #[serde(rename = "tree")]
    pub entries: Vec<TreeEntry>,
    pub truncated: bool,
}

fn git_url(repo: &RepoRef, path: &[&str], query: &[(&str, &str)]) -> Result<String, GithubError> {
    let mut segments = vec!["repos", &repo.owner, &repo.name, "git"];
    segments.extend(path.iter().copied());
    api_url(&segments, query)
}

// ブランチ名は/で分けてsegmentにする(check_ref_formatを通った名前なので、空の部分や..は無い)
fn ref_url(repo: &RepoRef, branch: &str) -> Result<String, GithubError> {
    let mut path = vec!["ref", "heads"];
    path.extend(branch.split('/'));
    git_url(repo, &path, &[])
}

// 404ならNone
pub fn branch_head(
    t: &dyn Transport,
    token: &str,
    now: i64,
    repo: &RepoRef,
    branch: &str,
) -> Result<Option<String>, GithubError> {
    let url = ref_url(repo, branch)?;
    let request = api_request(Method::Get, &url, token, None)?;
    match send_json(t, &request, now) {
        Ok((_, body)) => Ok(Some(string_of(&body, "object.sha")?)),
        Err(GithubError::Status { status: 404 }) => Ok(None),
        Err(error) => Err(error),
    }
}

// prefixは形式の{slug}より前。object.typeがcommitのものだけ。ページを最後までたどる
pub fn matching_branches(
    t: &dyn Transport,
    token: &str,
    now: i64,
    repo: &RepoRef,
    prefix: &str,
) -> Result<Vec<BranchRef>, GithubError> {
    let trimmed = prefix.trim_end_matches('/');
    let mut path = vec!["matching-refs", "heads"];
    if !trimmed.is_empty() {
        path.extend(trimmed.split('/'));
    }
    let mut url = git_url(repo, &path, &[("per_page", "100")])?;
    let mut branches = Vec::new();
    loop {
        let request = api_request(Method::Get, &url, token, None)?;
        let (response, body) = send_json(t, &request, now)?;
        let page = body
            .as_array()
            .ok_or_else(|| GithubError::invalid_field("refs"))?;
        for entry in page {
            // 名前はrefs/heads/{名前}の形。tagのrefはobject.typeがtagになるので除く
            if string_of(entry, "object.type")? != "commit" {
                continue;
            }
            let name = string_of(entry, "ref")?
                .strip_prefix("refs/heads/")
                .ok_or_else(|| GithubError::invalid_field("ref"))?
                .to_string();
            branches.push(BranchRef {
                name,
                commit: string_of(entry, "object.sha")?,
            });
        }
        url = match next_link(&response) {
            Some(next) => next,
            None => return Ok(branches),
        };
    }
}

pub fn commit_tree(
    t: &dyn Transport,
    token: &str,
    now: i64,
    repo: &RepoRef,
    commit: &str,
) -> Result<String, GithubError> {
    let url = git_url(repo, &["commits", commit], &[])?;
    let request = api_request(Method::Get, &url, token, None)?;
    let (_, body) = send_json(t, &request, now)?;
    string_of(&body, "tree.sha")
}

pub fn tree(
    t: &dyn Transport,
    token: &str,
    now: i64,
    repo: &RepoRef,
    sha: &str,
    recursive: bool,
) -> Result<Tree, GithubError> {
    let query: &[(&str, &str)] = if recursive {
        &[("recursive", "1")]
    } else {
        &[]
    };
    let url = git_url(repo, &["trees", sha], query)?;
    let request = api_request(Method::Get, &url, token, None)?;
    let (_, body) = send_json(t, &request, now)?;
    let entries = body
        .get("tree")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| GithubError::invalid_field("tree"))?;
    let entries = entries
        .iter()
        .map(|entry| {
            Ok(TreeEntry {
                path: string_of(entry, "path")?,
                mode: string_of(entry, "mode")?,
                kind: string_of(entry, "type")?,
                sha: string_of(entry, "sha")?,
            })
        })
        .collect::<Result<Vec<_>, GithubError>>()?;
    let truncated = body
        .get("truncated")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| GithubError::invalid_field("truncated"))?;
    Ok(Tree { entries, truncated })
}

// rawで取り、blob_shaが合わなければInvalidResponse
pub fn blob(
    t: &dyn Transport,
    token: &str,
    now: i64,
    repo: &RepoRef,
    sha: &str,
) -> Result<Vec<u8>, GithubError> {
    let url = git_url(repo, &["blobs", sha], &[])?;
    let request = api_request_accept(Method::Get, &url, token, RAW_ACCEPT)?;
    let content = send_raw(t, &request, now)?.body;
    if blob_sha(&content) != sha {
        return Err(GithubError::InvalidResponse {
            reason: "the blob sha does not match".to_string(),
        });
    }
    Ok(content)
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;
    use crate::github::fake::{blob_sha, matching_refs_url, repo_ref, url_of, RouteTransport};
    use crate::github::http::{Method, Request, Response};

    const TOKEN: &str = "TOKEN";
    const NOW: i64 = 1_000_000;

    fn last_request(transport: &RouteTransport) -> Request {
        transport
            .requests()
            .last()
            .cloned()
            .expect("通信はあるはず")
    }

    // Linkヘッダーで次のページのURLを教える応答
    fn page_with_next(transport: &RouteTransport, url: &str, page: serde_json::Value, next: &str) {
        let link = format!("<{next}>; rel=\"next\"");
        transport.on(
            Method::Get,
            url,
            Box::new(move |_| {
                Ok(Response {
                    status: 200,
                    headers: vec![("Link".to_string(), link.clone())],
                    body: serde_json::to_vec(&page).unwrap(),
                })
            }),
        );
    }

    fn ref_entry(branch: &str, sha: &str, kind: &str) -> serde_json::Value {
        serde_json::json!({
            "ref": format!("refs/heads/{branch}"),
            "object": { "sha": sha, "type": kind },
        })
    }

    #[test]
    fn matching_refsのURLはprefixごとに組み立てる() {
        for (prefix, prefix_segments) in [
            ("", vec![]),
            ("blog", vec!["blog"]),
            ("a/b-", vec!["a", "b-"]),
        ] {
            let url = matching_refs_url(&prefix_segments);
            let transport = RouteTransport::new();
            transport.json(Method::Get, &url, 200, serde_json::json!([]));

            let branches = matching_branches(&transport, TOKEN, NOW, &repo_ref(), prefix).unwrap();

            assert_eq!(branches, Vec::new());
            let request = last_request(&transport);
            assert_eq!(request.method, Method::Get);
            assert_eq!(request.url, url, "prefix: {prefix}");
            assert!(url.ends_with("?per_page=100"), "{url}");
        }
    }

    #[test]
    fn matching_refsはLinkのnextを最後までたどる() {
        let first = matching_refs_url(&["blog"]);
        let second = "https://api.github.com/repos/owner/blog/git/matching-refs/heads/blog?page=2"
            .to_string();
        let third = "https://api.github.com/repos/owner/blog/git/matching-refs/heads/blog?page=3"
            .to_string();

        let transport = RouteTransport::new();
        page_with_next(
            &transport,
            &first,
            serde_json::json!([ref_entry("blog/first-post", "sha-1", "commit")]),
            &second,
        );
        page_with_next(
            &transport,
            &second,
            serde_json::json!([ref_entry("blog/second-post", "sha-2", "commit")]),
            &third,
        );
        // 最後のページはLinkが無い
        transport.json(
            Method::Get,
            &third,
            200,
            serde_json::json!([ref_entry("blog/third-post", "sha-3", "commit")]),
        );

        let branches = matching_branches(&transport, TOKEN, NOW, &repo_ref(), "blog").unwrap();
        assert_eq!(
            branches,
            vec![
                BranchRef {
                    name: "blog/first-post".to_string(),
                    commit: "sha-1".to_string(),
                },
                BranchRef {
                    name: "blog/second-post".to_string(),
                    commit: "sha-2".to_string(),
                },
                BranchRef {
                    name: "blog/third-post".to_string(),
                    commit: "sha-3".to_string(),
                },
            ]
        );
        let urls: Vec<String> = transport.requests().iter().map(|r| r.url.clone()).collect();
        assert_eq!(urls, [first, second, third]);
    }

    #[test]
    fn matching_refsはobject_typeがcommitでないrefを使わない() {
        let transport = RouteTransport::new();
        transport.json(
            Method::Get,
            &matching_refs_url(&["blog"]),
            200,
            serde_json::json!([
                ref_entry("blog/tagged", "sha-tag", "tag"),
                ref_entry("blog/first-post", "sha-post", "commit"),
            ]),
        );

        let branches = matching_branches(&transport, TOKEN, NOW, &repo_ref(), "blog").unwrap();
        assert_eq!(
            branches,
            vec![BranchRef {
                name: "blog/first-post".to_string(),
                commit: "sha-post".to_string(),
            }]
        );
    }

    #[test]
    fn branch名のあるrefのURLはsegmentごとにエンコードする() {
        // / はsegmentの区切りのままで、# のような文字はpercentエンコードされる
        let target = "https://api.github.com/repos/owner/blog/git/ref/heads/feature/a%23b";
        let transport = RouteTransport::new();
        transport.json(
            Method::Get,
            target,
            200,
            serde_json::json!({"ref": "refs/heads/feature/a#b", "object": {"sha": "sha-f", "type": "commit"}}),
        );

        let commit = branch_head(&transport, TOKEN, NOW, &repo_ref(), "feature/a#b")
            .unwrap()
            .expect("コミットは取れるはず");
        assert_eq!(commit, "sha-f");
        let request = last_request(&transport);
        assert_eq!(request.url, target);
    }

    #[test]
    fn branch_headは404ならNoneで先頭のコミットを返す() {
        let transport = RouteTransport::new();
        let main_url = url_of(
            &["repos", "owner", "blog", "git", "ref", "heads", "main"],
            &[],
        );
        transport.json(
            Method::Get,
            &main_url,
            404,
            serde_json::json!({"message": "Not Found"}),
        );
        assert_eq!(
            branch_head(&transport, TOKEN, NOW, &repo_ref(), "main").unwrap(),
            None
        );

        let feature_url = url_of(
            &[
                "repos", "owner", "blog", "git", "ref", "heads", "feature", "x",
            ],
            &[],
        );
        transport.json(
            Method::Get,
            &feature_url,
            200,
            serde_json::json!({"ref": "refs/heads/feature/x", "object": {"sha": "sha-f", "type": "commit"}}),
        );
        assert_eq!(
            branch_head(&transport, TOKEN, NOW, &repo_ref(), "feature/x").unwrap(),
            Some("sha-f".to_string())
        );
    }

    #[test]
    fn treeはrecursiveの有無でURLを替えtruncatedを読む() {
        let transport = RouteTransport::new();
        let entry = TreeEntry {
            path: "01.png".to_string(),
            mode: "100644".to_string(),
            kind: "blob".to_string(),
            sha: "sha-1".to_string(),
        };
        let body = tree_body(&entry);

        // 再帰なし。URLにqueryは付かない
        transport.json(Method::Get, &tree_url(false), 200, body.clone());
        let value = tree(&transport, TOKEN, NOW, &repo_ref(), "tree-x", false).unwrap();
        assert_eq!(value.entries, vec![entry.clone()]);
        assert!(value.truncated);
        let request = last_request(&transport);
        assert_eq!(request.url, tree_url(false));

        // 再帰あり。URLに?recursive=1を付ける
        transport.json(Method::Get, &tree_url(true), 200, body);
        let value = tree(&transport, TOKEN, NOW, &repo_ref(), "tree-x", true).unwrap();
        assert_eq!(value.entries, vec![entry]);
        assert!(value.truncated);
        let request = last_request(&transport);
        assert_eq!(request.url, tree_url(true));
    }

    fn tree_url(recursive: bool) -> String {
        url_of(
            &["repos", "owner", "blog", "git", "trees", "tree-x"],
            if recursive {
                &[("recursive", "1")]
            } else {
                &[]
            },
        )
    }

    fn tree_body(entry: &TreeEntry) -> serde_json::Value {
        serde_json::json!({
            "sha": "tree-x",
            "tree": [
                {"path": entry.path, "mode": entry.mode, "type": entry.kind, "sha": entry.sha}
            ],
            "truncated": true,
        })
    }

    #[test]
    fn commit_treeはコミットのツリーのSHAを返す() {
        let transport = RouteTransport::new();
        let commits_url = url_of(&["repos", "owner", "blog", "git", "commits", "sha-c"], &[]);
        transport.json(
            Method::Get,
            &commits_url,
            200,
            serde_json::json!({"sha": "sha-c", "tree": {"sha": "tree-x"}}),
        );
        assert_eq!(
            commit_tree(&transport, TOKEN, NOW, &repo_ref(), "sha-c").unwrap(),
            "tree-x"
        );
        let request = last_request(&transport);
        assert_eq!(request.url, commits_url);
    }

    #[test]
    fn blobはrawで取ってSHAを確かめる() {
        let content = b"hello\n".to_vec();
        let sha = blob_sha(&content);
        let transport = RouteTransport::new();
        transport.raw(
            Method::Get,
            &url_of(
                &["repos", "owner", "blog", "git", "blobs", sha.as_str()],
                &[],
            ),
            200,
            content.clone(),
        );
        let body = blob(&transport, TOKEN, NOW, &repo_ref(), &sha).unwrap();
        assert_eq!(body, content);
        // Acceptはblobのraw
        let request = last_request(&transport);
        let accept = request
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("accept"))
            .map(|(_, value)| value.as_str())
            .expect("Acceptはあるはず");
        assert_eq!(accept, "application/vnd.github.raw+json");

        // SHAが合わない応答はInvalidResponseで、中身は返さない
        let other = blob_sha(b"other\n");
        transport.raw(
            Method::Get,
            &url_of(
                &["repos", "owner", "blog", "git", "blobs", other.as_str()],
                &[],
            ),
            200,
            b"mismatch\n".to_vec(),
        );
        assert!(matches!(
            blob(&transport, TOKEN, NOW, &repo_ref(), &other),
            Err(GithubError::InvalidResponse { .. })
        ));
    }
}
