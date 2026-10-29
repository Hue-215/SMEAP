use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use super::config::RepoRef;
use super::http::{api_url, Method, Request, Response, Transport};
use super::GithubError;

// テストで通信を置き換えるための偽のTransport。受けたリクエストを記録し、用意したレスポンスを順に返す
pub struct FakeTransport {
    responses: Mutex<VecDeque<Result<Response, GithubError>>>,
    requests: Mutex<Vec<Request>>,
}

impl FakeTransport {
    pub fn new(responses: Vec<Result<Response, GithubError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Transport for FakeTransport {
    fn send(&self, request: &Request) -> Result<Response, GithubError> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.clone());
        self.responses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .expect("用意したレスポンスが足りない")
    }
}

// ディレクトリを書き込めなくし、落とすときに戻す
#[cfg(unix)]
pub struct ReadOnlyDir(std::path::PathBuf);

#[cfg(unix)]
impl Drop for ReadOnlyDir {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
    }
}

// rootなど、権限を外しても書き込めてしまう環境ではNone(書き込みの失敗を試すテストはそこで終える)
#[cfg(unix)]
pub fn read_only_dir(dir: &std::path::Path) -> Option<ReadOnlyDir> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let guard = ReadOnlyDir(dir.to_path_buf());
    let probe = dir.join(".probe");
    if std::fs::write(&probe, "").is_ok() {
        let _ = std::fs::remove_file(probe);
        return None;
    }
    Some(guard)
}

pub fn json_response(status: u16, body: serde_json::Value) -> Response {
    Response {
        status,
        headers: Vec::new(),
        body: serde_json::to_vec(&body).expect("JSONの応答は書けるはず"),
    }
}

// git.rs・remote.rs・sync.rsのテストで使うRepository
pub fn repo_ref() -> RepoRef {
    RepoRef {
        owner: "owner".to_string(),
        name: "blog".to_string(),
        default_branch: "main".to_string(),
    }
}

pub fn url_of(segments: &[&str], query: &[(&str, &str)]) -> String {
    api_url(segments, query).unwrap()
}

pub fn matching_refs_url(prefix_segments: &[&str]) -> String {
    let mut segments = vec!["repos", "owner", "blog", "git", "matching-refs", "heads"];
    segments.extend(prefix_segments);
    url_of(&segments, &[("per_page", "100")])
}

pub type Handler = Box<dyn Fn(&Request) -> Result<Response, GithubError> + Send + Sync>;

// 応答の関数はロックの外で呼ぶ。テストのhandlerの中で登録や状態の書き換えができるようにするため
type Route = Arc<dyn Fn(&Request) -> Result<Response, GithubError> + Send + Sync>;

// (Method, URL)ごとに応答の関数を返す。受けたリクエストは記録し、登録の無いものはpanicにする
// MethodにはHashとOrdが無いので、登録はVecに入れて探す
pub struct RouteTransport {
    handlers: Mutex<Vec<((Method, String), Route)>>,
    requests: Mutex<Vec<Request>>,
}

impl RouteTransport {
    pub fn new() -> Self {
        Self {
            handlers: Mutex::new(Vec::new()),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn on(&self, method: Method, url: &str, handler: Handler) {
        let route: Route = Arc::from(handler);
        let mut handlers = self.handlers.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (method, url.to_string());
        if let Some(entry) = handlers
            .iter_mut()
            .find(|(registered, _)| *registered == key)
        {
            entry.1 = route;
        } else {
            handlers.push((key, route));
        }
    }

    // 決まった応答を返す短い形
    pub fn json(&self, method: Method, url: &str, status: u16, body: serde_json::Value) {
        let response = json_response(status, body);
        self.on(method, url, Box::new(move |_| Ok(response.clone())));
    }

    pub fn raw(&self, method: Method, url: &str, status: u16, body: Vec<u8>) {
        let response = Response {
            status,
            headers: Vec::new(),
            body,
        };
        self.on(method, url, Box::new(move |_| Ok(response.clone())));
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Transport for RouteTransport {
    fn send(&self, request: &Request) -> Result<Response, GithubError> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.clone());
        let handler = self
            .handlers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(key, _)| *key == (request.method, request.url.clone()))
            .map(|(_, handler)| handler.clone())
            .expect("登録の無いリクエストです");
        handler(request)
    }
}

// "blob {len}\0{content}"のSHA-1を16進の小文字で。テストの期待をstate::blob_shaに頼らないよう、
// ここで別に求める
pub fn blob_sha(content: &[u8]) -> String {
    use sha1::{Digest, Sha1};

    let mut hasher = Sha1::new();
    hasher.update(b"blob ");
    hasher.update(content.len().to_string().as_bytes());
    hasher.update([0]);
    hasher.update(content);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// パスの連なり。filesのパスから組み立てて、gitのツリーの形にして登録する
#[derive(Default)]
struct TreeNode {
    children: BTreeMap<String, Node>,
}

enum Node {
    Dir(TreeNode),
    Blob {
        mode: String,
        content: Vec<u8>,
    },
    // サブモジュールなど、blobでないもの
    Other {
        mode: String,
        kind: String,
        sha: String,
    },
}

// recursiveの応答を作り直すための、ツリーのエントリ
struct RefEntry {
    path: String,
    mode: String,
    kind: String,
    sha: String,
}

fn insert_path(root: &mut TreeNode, path: &str, leaf: Node) {
    let mut components: Vec<&str> = path.split('/').collect();
    let last = components.pop().expect("パスが空");
    let mut node = root;
    for component in components {
        let entry = node
            .children
            .entry(component.to_string())
            .or_insert_with(|| Node::Dir(TreeNode::default()));
        node = match entry {
            Node::Dir(dir) => dir,
            Node::Blob { .. } | Node::Other { .. } => {
                *entry = Node::Dir(TreeNode::default());
                match entry {
                    Node::Dir(dir) => dir,
                    _ => unreachable!("直前に置き換えたはず"),
                }
            }
        };
    }
    node.children.insert(last.to_string(), leaf);
}

fn tree_entry(path: &str, mode: &str, kind: &str, sha: &str) -> serde_json::Value {
    serde_json::json!({ "path": path, "mode": mode, "type": kind, "sha": sha })
}

fn ref_json(branch: &str, commit: &str) -> serde_json::Value {
    serde_json::json!({
        "ref": format!("refs/heads/{branch}"),
        "object": { "sha": commit, "type": "commit" },
    })
}

fn not_found() -> Response {
    json_response(404, serde_json::json!({ "message": "Not Found" }))
}

// matching-refsはprefixごとにURLが替わる。GitHubは渡した文字列で始まるrefを、/の区切りによらず
// 返すので、前方一致で選ぶ
fn starts_with_prefix(branch: &str, prefix: &str) -> bool {
    prefix.is_empty() || branch.starts_with(prefix)
}

// prefix候補。slug(validate_slugと同じ形)が始まり得る位置と、segmentの区切り。
// 実装は末尾の/を落としてsegmentに分けるので、prefixからも落としておく
fn candidate_prefixes(branch: &str) -> Vec<String> {
    let mut prefixes = BTreeSet::new();
    prefixes.insert(String::new());
    for (index, _) in branch.char_indices() {
        let prefix = branch[..index].trim_end_matches('/');
        let suffix = &branch[index..];
        let slug_ok = !suffix.is_empty()
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if slug_ok || branch.as_bytes().get(index) == Some(&b'/') {
            prefixes.insert(prefix.to_string());
        }
    }
    prefixes.into_iter().collect()
}

// GitHub上のリポジトリを組み立てるヘルパー。ブランチ名 -> 先頭コミットのファイル(リポジトリの
// ルートからのパス -> 中身)で作り、git/ref・matching-refs・git/commits・git/trees・git/blobsの
// 応答をRouteTransportに登録する。同じ中身でも呼ぶたびに別のコミットのSHAにする。
// 状態は共有されているので、クローンして渡しても応答は同じになる
#[derive(Clone)]
pub struct FakeRepo {
    owner: String,
    name: String,
    // ブランチ(refs/heads/を除いた名前) -> 先頭のコミット
    branches: Arc<Mutex<BTreeMap<String, String>>>,
    // コミット -> ルートのツリー。ブランチを消すと、コミットの応答も404にする
    commits: Arc<Mutex<BTreeMap<String, String>>>,
    counter: Arc<AtomicU64>,
}

impl FakeRepo {
    pub fn new(transport: &RouteTransport, owner: &str, name: &str) -> Self {
        let repo = Self {
            owner: owner.to_string(),
            name: name.to_string(),
            branches: Arc::new(Mutex::new(BTreeMap::new())),
            commits: Arc::new(Mutex::new(BTreeMap::new())),
            counter: Arc::new(AtomicU64::new(1)),
        };
        // 空のprefixと既定の形式(blog/{slug})のmatching-refsは、記事用のブランチが無いときにも使う
        repo.register_matching_refs(transport, "");
        repo.register_matching_refs(transport, "blog");
        repo
    }

    // ブランチを作る・替える。コミット・ツリー・blobの応答を登録し、コミットのSHAを返す
    pub fn branch(
        &self,
        transport: &RouteTransport,
        branch: &str,
        files: &[(&str, &[u8])],
    ) -> String {
        self.branch_with_entries(transport, branch, files, &[])
    }

    // ツリーに、blobでないもの(mode・type)を入れる
    pub fn branch_with_entries(
        &self,
        transport: &RouteTransport,
        branch: &str,
        files: &[(&str, &[u8])],
        extra: &[(&str, &str, &str)],
    ) -> String {
        let mut root = TreeNode::default();
        for (path, content) in files {
            insert_path(
                &mut root,
                path,
                Node::Blob {
                    mode: "100644".to_string(),
                    content: content.to_vec(),
                },
            );
        }
        for (path, mode, kind) in extra {
            let node = match *kind {
                // 中身の無いディレクトリ
                "tree" => Node::Dir(TreeNode::default()),
                "commit" => Node::Other {
                    mode: mode.to_string(),
                    kind: kind.to_string(),
                    sha: format!("sub-{}", self.counter.fetch_add(1, Ordering::SeqCst)),
                },
                // シンボリックリンクなど。中身は空にする
                _ => Node::Blob {
                    mode: mode.to_string(),
                    content: Vec::new(),
                },
            };
            insert_path(&mut root, path, node);
        }

        let (root_sha, _) = self.register_tree(transport, &root);
        let commit_sha = format!("commit-{}", self.counter.fetch_add(1, Ordering::SeqCst));
        self.commits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(commit_sha.clone(), root_sha);
        self.branches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(branch.to_string(), commit_sha.clone());
        self.register_commit(transport, &commit_sha);
        self.register_refs(transport, branch);
        commit_sha
    }

    pub fn delete_branch(&self, branch: &str) {
        let mut branches = self.branches.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(commit) = branches.remove(branch) {
            // 先頭だったコミットも、ブランチと一緒に読めなくなる
            self.commits
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&commit);
        }
    }

    // ツリーを登録し、そのSHAと、再帰の応答の分のエントリ(パスはそのツリーからの相対)を返す。
    // GitHubはrecursiveの応答のpathを、読んだツリーからの相対パスで返す
    fn register_tree(
        &self,
        transport: &RouteTransport,
        node: &TreeNode,
    ) -> (String, Vec<RefEntry>) {
        let mut immediate: Vec<serde_json::Value> = Vec::new();
        let mut recursive: Vec<RefEntry> = Vec::new();
        for (name, child) in &node.children {
            let (mode, kind, sha, sub) = match child {
                Node::Dir(dir) => {
                    let (sha, sub) = self.register_tree(transport, dir);
                    ("040000".to_string(), "tree".to_string(), sha, sub)
                }
                Node::Blob { mode, content } => {
                    let sha = blob_sha(content);
                    transport.raw(
                        Method::Get,
                        &self.url("blobs", &[sha.as_str()], &[]),
                        200,
                        content.clone(),
                    );
                    (mode.clone(), "blob".to_string(), sha, Vec::new())
                }
                Node::Other { mode, kind, sha } => {
                    (mode.clone(), kind.clone(), sha.clone(), Vec::new())
                }
            };
            immediate.push(tree_entry(name, &mode, &kind, &sha));
            recursive.push(RefEntry {
                path: name.clone(),
                mode,
                kind,
                sha,
            });
            recursive.extend(sub.into_iter().map(|entry| RefEntry {
                path: format!("{name}/{}", entry.path),
                ..entry
            }));
        }
        let sha = format!("tree-{}", self.counter.fetch_add(1, Ordering::SeqCst));
        let recursive_entries: Vec<serde_json::Value> = recursive
            .iter()
            .map(|entry| tree_entry(&entry.path, &entry.mode, &entry.kind, &entry.sha))
            .collect();
        transport.json(
            Method::Get,
            &self.url("trees", &[sha.as_str()], &[]),
            200,
            serde_json::json!({
                "sha": sha,
                "tree": immediate,
                "truncated": false,
            }),
        );
        transport.json(
            Method::Get,
            &self.url("trees", &[sha.as_str()], &[("recursive", "1")]),
            200,
            serde_json::json!({
                "sha": sha,
                "tree": recursive_entries,
                "truncated": false,
            }),
        );
        (sha, recursive)
    }

    fn register_commit(&self, transport: &RouteTransport, commit_sha: &str) {
        let url = self.url("commits", &[commit_sha], &[]);
        let commits = self.commits.clone();
        let sha = commit_sha.to_string();
        transport.on(
            Method::Get,
            &url,
            Box::new(move |_| {
                let commits = commits.lock().unwrap_or_else(PoisonError::into_inner);
                match commits.get(&sha) {
                    Some(tree) => Ok(json_response(
                        200,
                        serde_json::json!({ "sha": sha, "tree": { "sha": tree } }),
                    )),
                    None => Ok(not_found()),
                }
            }),
        );
    }

    // ブランチを追加・削除したときに替わる応答は、毎回状態を読んで返す
    fn register_refs(&self, transport: &RouteTransport, branch: &str) {
        let url = self.url(
            "ref/heads",
            branch.split('/').collect::<Vec<_>>().as_slice(),
            &[],
        );
        let branches = self.branches.clone();
        let name = branch.to_string();
        transport.on(
            Method::Get,
            &url,
            Box::new(move |_| {
                let branches = branches.lock().unwrap_or_else(PoisonError::into_inner);
                match branches.get(&name) {
                    Some(commit) => Ok(json_response(200, ref_json(&name, commit))),
                    None => Ok(not_found()),
                }
            }),
        );

        // 実装は形式の{slug}より前でブランチの一覧を取る。prefixはブランチ名だけからは
        // 一意に決まらないので、slugが乗り得る位置のprefixを登録しておく。足りない分はテストで
        // transport.onに上書きする
        for prefix in candidate_prefixes(branch) {
            self.register_matching_refs(transport, &prefix);
        }
    }

    fn register_matching_refs(&self, transport: &RouteTransport, prefix: &str) {
        let segments: Vec<&str> = if prefix.is_empty() {
            Vec::new()
        } else {
            prefix.split('/').collect()
        };
        let url = self.url("matching-refs/heads", &segments, &[("per_page", "100")]);
        let branches = self.branches.clone();
        let prefix = prefix.to_string();
        transport.on(
            Method::Get,
            &url,
            Box::new(move |_| {
                let branches = branches.lock().unwrap_or_else(PoisonError::into_inner);
                let refs: Vec<serde_json::Value> = branches
                    .iter()
                    .filter(|(branch, _)| starts_with_prefix(branch, &prefix))
                    .map(|(branch, commit)| ref_json(branch, commit))
                    .collect();
                Ok(json_response(200, serde_json::Value::Array(refs)))
            }),
        );
    }

    // APIのURLを組み立てる。pathはgit/の後ろの部分
    fn url(&self, path: &str, segments: &[&str], query: &[(&str, &str)]) -> String {
        let mut all = vec!["repos", self.owner.as_str(), self.name.as_str(), "git"];
        all.extend(path.split('/'));
        all.extend(segments.iter().copied());
        api_url(&all, query).expect("fakeのURLは組み立てられるはず")
    }
}
