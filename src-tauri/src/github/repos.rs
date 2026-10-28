use serde::Serialize;

use super::http::{api_request, api_url, classify, json_body, next_link, Method, Transport};
use super::GithubError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Repository {
    pub owner: String,
    pub name: String,
    pub default_branch: String,
}

pub fn current_user(
    transport: &dyn Transport,
    token: &str,
    now: i64,
) -> Result<String, GithubError> {
    let request = api_request(Method::Get, &api_url(&["user"], &[])?, token, None)?;
    let response = classify(transport.send(&request)?, now)?;
    let body = json_body(&response)?;
    body.get("login")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| GithubError::invalid_field("login"))
}

pub fn list_repositories(
    transport: &dyn Transport,
    token: &str,
    now: i64,
) -> Result<Vec<Repository>, GithubError> {
    let start = api_url(&["user", "installations"], &[("per_page", "100")])?;
    let mut repositories = Vec::new();
    for installation in list_all(transport, token, now, &start, "installations")? {
        let id = installation
            .get("id")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| GithubError::invalid_field("id"))?;
        let start = api_url(
            &["user", "installations", &id.to_string(), "repositories"],
            &[("per_page", "100")],
        )?;
        for item in list_all(transport, token, now, &start, "repositories")? {
            repositories.push(repository_of(&item)?);
        }
    }
    // installationsの順に集めたので、重なりを1つにしてowner/nameの順に並べ替える
    repositories.sort_by(|a, b| (&a.owner, &a.name).cmp(&(&b.owner, &b.name)));
    repositories.dedup_by(|a, b| a.owner == b.owner && a.name == b.name);
    Ok(repositories)
}

// 配列の項目のJSONを、Linkのnextをたどって全ページ分集める
fn list_all(
    transport: &dyn Transport,
    token: &str,
    now: i64,
    start_url: &str,
    key: &str,
) -> Result<Vec<serde_json::Value>, GithubError> {
    let mut items = Vec::new();
    // 2ページ目以降は、Linkのnextに書かれたURLをそのまま使う
    let mut url = Some(start_url.to_string());
    while let Some(current) = url {
        let request = api_request(Method::Get, &current, token, None)?;
        let response = classify(transport.send(&request)?, now)?;
        let body = json_body(&response)?;
        let page = body
            .get(key)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| GithubError::invalid_field(key))?;
        items.extend(page.iter().cloned());
        url = next_link(&response);
    }
    Ok(items)
}

fn repository_of(item: &serde_json::Value) -> Result<Repository, GithubError> {
    Ok(Repository {
        owner: item
            .get("owner")
            .and_then(|owner| owner.get("login"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| GithubError::invalid_field("owner.login"))?
            .to_string(),
        name: item
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| GithubError::invalid_field("name"))?
            .to_string(),
        default_branch: item
            .get("default_branch")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| GithubError::invalid_field("default_branch"))?
            .to_string(),
    })
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::github::fake::{json_response, FakeTransport};
    use crate::github::http::Response;

    const NOW: i64 = 1_000_000;
    const TOKEN: &str = "TOKEN";

    fn repository(full_name: &str) -> Repository {
        let (owner, name) = full_name.split_once('/').unwrap();
        Repository {
            owner: owner.to_string(),
            name: name.to_string(),
            default_branch: "main".to_string(),
        }
    }

    fn repositories_page(full_names: &[&str]) -> serde_json::Value {
        json!({
            "repositories": full_names
                .iter()
                .map(|full_name| {
                    let (owner, name) = full_name.split_once('/').unwrap();
                    json!({
                        "name": name,
                        "full_name": full_name,
                        "owner": { "login": owner },
                        "default_branch": "main"
                    })
                })
                .collect::<Vec<_>>()
        })
    }

    // 次のページへのLinkヘッダー。応答の順に返すので、FakeTransportはURLを見ない
    fn link_of(url: &str, page: u32) -> (String, String) {
        (
            "Link".to_string(),
            format!(r#"<{url}?page={page}>; rel="next""#),
        )
    }

    fn installations(ids: &[i64]) -> serde_json::Value {
        json!({
            "installations": ids.iter().map(|id| json!({ "id": id })).collect::<Vec<_>>()
        })
    }

    #[test]
    fn current_userはuserのloginを返す() {
        let transport =
            FakeTransport::new(vec![Ok(json_response(200, json!({ "login": "USER" })))]);
        assert_eq!(current_user(&transport, TOKEN, NOW).unwrap(), "USER");
        assert_eq!(transport.requests()[0].url, "https://api.github.com/user");
    }

    #[test]
    fn repositoryの一覧はinstallationsとそれぞれのrepositoriesから作る() {
        let transport = FakeTransport::new(vec![
            Ok(json_response(200, installations(&[101, 102]))),
            Ok(json_response(200, repositories_page(&["owner/blog"]))),
            Ok(json_response(200, repositories_page(&["other/wiki"]))),
        ]);
        assert_eq!(
            list_repositories(&transport, TOKEN, NOW).unwrap(),
            vec![repository("other/wiki"), repository("owner/blog")]
        );
        let requests = transport.requests();
        // ページの大きさなどのqueryは問わない
        let urls: Vec<_> = requests
            .iter()
            .map(|request| request.url.split('?').next().unwrap())
            .collect();
        assert_eq!(
            urls,
            vec![
                "https://api.github.com/user/installations",
                "https://api.github.com/user/installations/101/repositories",
                "https://api.github.com/user/installations/102/repositories",
            ]
        );
    }

    #[test]
    fn 一覧はLinkのnextをたどって全ページをつなげる() {
        // installationの一覧が2ページ、101のrepositoryの一覧も2ページ
        let transport = FakeTransport::new(vec![
            Ok(Response {
                status: 200,
                headers: vec![link_of("https://api.github.com/user/installations", 2)],
                body: serde_json::to_vec(&installations(&[101])).unwrap(),
            }),
            Ok(json_response(200, installations(&[102]))),
            Ok(Response {
                status: 200,
                headers: vec![link_of(
                    "https://api.github.com/user/installations/101/repositories",
                    2,
                )],
                body: serde_json::to_vec(&repositories_page(&["owner/blog"])).unwrap(),
            }),
            Ok(json_response(200, repositories_page(&["owner/other"]))),
            Ok(json_response(200, repositories_page(&["third/app"]))),
        ]);
        assert_eq!(
            list_repositories(&transport, TOKEN, NOW).unwrap(),
            vec![
                repository("owner/blog"),
                repository("owner/other"),
                repository("third/app"),
            ]
        );
        // 2ページ目は、Linkのnextに書かれたURLをそのまま使う
        let urls: Vec<_> = transport
            .requests()
            .into_iter()
            .map(|request| request.url)
            .collect();
        assert_eq!(urls.len(), 5);
        assert_eq!(urls[1], "https://api.github.com/user/installations?page=2");
        assert_eq!(
            urls[3],
            "https://api.github.com/user/installations/101/repositories?page=2"
        );
    }

    #[test]
    fn 重なるrepositoryは1つにしてowner_nameの順に並べる() {
        let transport = FakeTransport::new(vec![
            Ok(json_response(200, installations(&[101, 102]))),
            Ok(json_response(
                200,
                repositories_page(&["beta/app", "alpha/blog"]),
            )),
            Ok(json_response(
                200,
                repositories_page(&["alpha/blog", "charlie/doc"]),
            )),
        ]);
        let list = list_repositories(&transport, TOKEN, NOW).unwrap();
        let names: Vec<_> = list
            .iter()
            .map(|repository| format!("{}/{}", repository.owner, repository.name))
            .collect();
        assert_eq!(names, ["alpha/blog", "beta/app", "charlie/doc"]);
    }

    #[test]
    fn installationが無ければ空の一覧() {
        let transport = FakeTransport::new(vec![Ok(json_response(200, installations(&[])))]);
        assert_eq!(
            list_repositories(&transport, TOKEN, NOW).unwrap(),
            Vec::new()
        );
        assert_eq!(transport.requests().len(), 1);
    }

    #[test]
    fn 途中のページで失敗したら一覧を返さずにエラー() {
        let transport = FakeTransport::new(vec![
            Ok(json_response(200, installations(&[101]))),
            Ok(json_response(404, json!({}))),
        ]);
        assert!(list_repositories(&transport, TOKEN, NOW).is_err());
    }

    #[test]
    fn APIが401ならログインし直しを返す() {
        let transport = FakeTransport::new(vec![Ok(json_response(401, json!({})))]);
        let error = current_user(&transport, TOKEN, NOW).unwrap_err();
        assert!(matches!(error, GithubError::LoginRequired));
    }
}
