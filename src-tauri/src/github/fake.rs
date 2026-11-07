use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use super::config::{token_path, RepoRef};
use super::http::{api_url, Method, Request, Response, Transport, API_BASE};
use super::token::{SecretStore, TokenSet, TokenStore};
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

// compareのURL。baseheadの...はapi_urlが..として断るので、ここで組み立てる
pub fn compare_url(base: &str, head: &str) -> String {
    format!("{API_BASE}/repos/owner/blog/compare/{base}...{head}")
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
#[derive(Default, Clone)]
struct TreeNode {
    children: BTreeMap<String, Node>,
}

#[derive(Clone)]
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

// 書き込みの通信の、n回目の応答の前に関数を呼ぶためのもの
pub type Hook = Arc<dyn Fn() + Send + Sync>;

fn hook_key(method: Method, url: &str) -> String {
    format!("{method:?} {url}")
}

fn json_of(request: &Request) -> serde_json::Value {
    serde_json::from_slice(request.body.as_deref().unwrap_or(&[]))
        .expect("書き込みの本文はJSONのはず")
}

// base64を解く
pub fn b64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buffer: u32 = 0;
    let mut bits = 0usize;
    for character in text.chars() {
        let value = match character {
            'A'..='Z' => character as u32 - 'A' as u32,
            'a'..='z' => 26 + character as u32 - 'a' as u32,
            '0'..='9' => 52 + character as u32 - '0' as u32,
            '+' => 62,
            '/' => 63,
            // padding。末尾にあるはずだが、そこまで読めば十分
            '=' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

// パスのファイルを外し、その上に空になったディレクトリがあれば消す
// 無いパスならfalse
fn remove_path(root: &mut TreeNode, path: &str) -> bool {
    let mut components: Vec<&str> = path.split('/').collect();
    let last = components.pop().expect("パスが空");
    let mut node = root;
    for component in components {
        match node.children.get_mut(component) {
            Some(Node::Dir(dir)) => node = dir,
            _ => return false,
        }
    }
    let removed = node.children.remove(last).is_some();
    prune(node);
    removed
}

// 空のディレクトリはgitのツリーに無いので、外す
fn prune(node: &mut TreeNode) {
    let names: Vec<String> = node.children.keys().cloned().collect();
    for name in names {
        let empty = match node.children.get_mut(&name) {
            Some(Node::Dir(dir)) => {
                prune(dir);
                dir.children.is_empty()
            }
            _ => false,
        };
        if empty {
            node.children.remove(&name);
        }
    }
}

// ルートのツリーを下りて、blobのファイル(パス -> mode・中身)を集める
fn collect_files(node: &TreeNode, prefix: &str, files: &mut BTreeMap<String, (String, Vec<u8>)>) {
    for (name, child) in &node.children {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        match child {
            Node::Dir(dir) => collect_files(dir, &path, files),
            Node::Blob { mode, content } => {
                files.insert(path.clone(), (mode.clone(), content.clone()));
            }
            Node::Other { .. } => {}
        }
    }
}

// コミットとその親から、そのコミットまでの祖先を集める
fn ancestors_of(commit: &str, parents: &BTreeMap<String, Vec<String>>) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([commit.to_string()]);
    while let Some(current) = queue.pop_front() {
        if !seen.insert(current.clone()) {
            continue;
        }
        if let Some(parents) = parents.get(&current) {
            queue.extend(parents.iter().cloned());
        }
    }
    seen
}

// 共通の祖先のうち、headからたどって最初に当たるもの
fn merge_base(base: &str, head: &str, parents: &BTreeMap<String, Vec<String>>) -> Option<String> {
    let base_ancestors = ancestors_of(base, parents);
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([head.to_string()]);
    while let Some(current) = queue.pop_front() {
        if !seen.insert(current.clone()) {
            continue;
        }
        if base_ancestors.contains(&current) {
            return Some(current);
        }
        if let Some(parents) = parents.get(&current) {
            queue.extend(parents.iter().cloned());
        }
    }
    None
}

// 2つのツリーのblobを比べた、added・removed・modifiedのファイル
fn compare_files(before: &TreeNode, after: &TreeNode) -> Vec<serde_json::Value> {
    let mut before_files = BTreeMap::new();
    collect_files(before, "", &mut before_files);
    let mut after_files = BTreeMap::new();
    collect_files(after, "", &mut after_files);
    let paths: BTreeSet<&String> = before_files.keys().chain(after_files.keys()).collect();
    let mut files = Vec::new();
    for path in paths {
        let status = match (before_files.get(path), after_files.get(path)) {
            (None, Some(_)) => "added",
            (Some(_), None) => "removed",
            (Some(before), Some(after)) if before != after => "modified",
            _ => continue,
        };
        files.push(serde_json::json!({ "filename": path, "status": status }));
    }
    files
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
    // 登録したツリー(sha -> 中身)。書き込みのbase_treeと、branchの中身の読みに使う
    trees: Arc<Mutex<BTreeMap<String, TreeNode>>>,
    // 書き込みで作ったblob(sha -> 中身)
    blobs: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    // コミットの親(PATCHの早送りの判定とcompareの分岐点に使う)。branch()で作ったコミットは入らない
    parents: Arc<Mutex<BTreeMap<String, Vec<String>>>>,
    // orphan_branchで作った、ほかの履歴とつながらないコミット。branch()で作った親の無いコミットは、
    // compareでは空のツリーの共通の祖先から切ったものとして扱う(実際のブランチは祖先を共有するため)
    orphans: Arc<Mutex<BTreeSet<String>>>,
    // 書き込みの通信を受け付けている間、register_refsはPATCHの応答も足す
    writes_on: Arc<AtomicBool>,
    // 書き込みの通信の、(Method, URL)ごとの出現回数
    counts: Arc<Mutex<BTreeMap<String, usize>>>,
    // 書き込みの通信の、n回目の応答の前に関数
    hooks: Arc<Mutex<BTreeMap<String, BTreeMap<usize, Hook>>>>,
    counter: Arc<AtomicU64>,
}

impl FakeRepo {
    pub fn new(transport: &RouteTransport, owner: &str, name: &str) -> Self {
        let repo = Self {
            owner: owner.to_string(),
            name: name.to_string(),
            branches: Arc::new(Mutex::new(BTreeMap::new())),
            commits: Arc::new(Mutex::new(BTreeMap::new())),
            trees: Arc::new(Mutex::new(BTreeMap::new())),
            blobs: Arc::new(Mutex::new(BTreeMap::new())),
            parents: Arc::new(Mutex::new(BTreeMap::new())),
            orphans: Arc::new(Mutex::new(BTreeSet::new())),
            writes_on: Arc::new(AtomicBool::new(false)),
            counts: Arc::new(Mutex::new(BTreeMap::new())),
            hooks: Arc::new(Mutex::new(BTreeMap::new())),
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
        self.branch_at(transport, branch, files, &[], None)
    }

    // 親を持つコミットでブランチを作るか進める
    pub fn branch_from(
        &self,
        transport: &RouteTransport,
        branch: &str,
        parent_commit: &str,
        files: &[(&str, &[u8])],
    ) -> String {
        self.branch_at(transport, branch, files, &[], Some(parent_commit))
    }

    // ほかのどのコミットとも共通の祖先を持たないブランチ
    pub fn orphan_branch(
        &self,
        transport: &RouteTransport,
        branch: &str,
        files: &[(&str, &[u8])],
    ) -> String {
        let commit = self.branch_at(transport, branch, files, &[], None);
        self.orphans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(commit.clone());
        self.register_compares(transport);
        commit
    }

    // ツリーに、blobでないもの(mode・type)を入れる
    pub fn branch_with_entries(
        &self,
        transport: &RouteTransport,
        branch: &str,
        files: &[(&str, &[u8])],
        extra: &[(&str, &str, &str)],
    ) -> String {
        self.branch_at(transport, branch, files, extra, None)
    }

    fn branch_at(
        &self,
        transport: &RouteTransport,
        branch: &str,
        files: &[(&str, &[u8])],
        extra: &[(&str, &str, &str)],
        parent: Option<&str>,
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
        if let Some(parent) = parent {
            self.parents
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(commit_sha.clone(), vec![parent.to_string()]);
        }
        self.branches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(branch.to_string(), commit_sha.clone());
        self.register_commit(transport, &commit_sha);
        self.register_refs(transport, branch);
        self.register_compares(transport);
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
                    // 書き込みのツリーのエントリで、既にあるblobのSHAも指せるようにする
                    self.blobs
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(sha.clone(), content.clone());
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
        // ツリーの中身を覚える(書き込みのbase_treeと、branchの中身の読みに使う)
        self.trees
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(sha.clone(), node.clone());
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
        let parents = self.parents.clone();
        let sha = commit_sha.to_string();
        transport.on(
            Method::Get,
            &url,
            Box::new(move |_| {
                let commits = commits.lock().unwrap_or_else(PoisonError::into_inner);
                match commits.get(&sha) {
                    Some(tree) => {
                        let parents = parents
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .get(&sha)
                            .cloned()
                            .unwrap_or_default();
                        Ok(json_response(
                            200,
                            serde_json::json!({
                                "sha": sha,
                                "tree": { "sha": tree },
                                "parents": parents.iter().map(|sha| serde_json::json!({ "sha": sha })).collect::<Vec<_>>(),
                            }),
                        ))
                    }
                    None => Ok(not_found()),
                }
            }),
        );
    }

    // compareの応答を、今あるコミットの組ごとに登録する。orphan_branchのコミットとの組は404
    fn register_compares(&self, transport: &RouteTransport) {
        let commits: Vec<String> = self
            .commits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .cloned()
            .collect();
        for base in &commits {
            for head in &commits {
                self.register_compare(transport, base, head);
            }
        }
    }

    fn register_compare(&self, transport: &RouteTransport, base: &str, head: &str) {
        let url = format!(
            "{API_BASE}/repos/{}/{}/compare/{base}...{head}",
            self.owner, self.name
        );
        let commits = Arc::clone(&self.commits);
        let trees = Arc::clone(&self.trees);
        let parents = Arc::clone(&self.parents);
        let orphans = Arc::clone(&self.orphans);
        let base = base.to_string();
        let head = head.to_string();
        transport.on(
            Method::Get,
            &url,
            Box::new(move |_| {
                let commits = commits.lock().unwrap_or_else(PoisonError::into_inner);
                if !commits.contains_key(&base) {
                    return Ok(not_found());
                }
                let Some(head_tree) = commits.get(&head) else {
                    return Ok(not_found());
                };
                let parents = parents.lock().unwrap_or_else(PoisonError::into_inner);
                let trees = trees.lock().unwrap_or_else(PoisonError::into_inner);
                let empty = TreeNode::default();
                let before = match merge_base(&base, &head, &parents) {
                    Some(merge) => {
                        let Some(tree) = commits.get(&merge).and_then(|sha| trees.get(sha)) else {
                            return Ok(not_found());
                        };
                        tree
                    }
                    None => {
                        let orphans = orphans.lock().unwrap_or_else(PoisonError::into_inner);
                        let rooted_in_orphan = |commit: &str| {
                            ancestors_of(commit, &parents)
                                .iter()
                                .any(|ancestor| orphans.contains(ancestor))
                        };
                        if rooted_in_orphan(&base) || rooted_in_orphan(&head) {
                            return Ok(not_found());
                        }
                        &empty
                    }
                };
                let Some(after) = trees.get(head_tree) else {
                    return Ok(not_found());
                };
                let files = compare_files(before, after);
                let base_ancestors = ancestors_of(&base, &parents);
                let head_ancestors = ancestors_of(&head, &parents);
                let behind_by = base_ancestors.difference(&head_ancestors).count();
                let ahead_by = head_ancestors.difference(&base_ancestors).count();
                Ok(json_response(
                    200,
                    serde_json::json!({
                        "status": "diverged",
                        "ahead_by": ahead_by,
                        "behind_by": behind_by,
                        "total_commits": ahead_by,
                        "files": files,
                    }),
                ))
            }),
        );
    }

    // ブランチの先頭を、書き込みで作ったコミットにする(届いたのに失敗と応答した場合の再現)
    pub fn point_branch(&self, transport: &RouteTransport, branch: &str, commit: &str) {
        self.branches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(branch.to_string(), commit.to_string());
        self.register_refs(transport, branch);
    }

    // まだ無いブランチのrefの応答(無ければ404)を登録する
    pub fn watch_ref(&self, transport: &RouteTransport, branch: &str) {
        self.register_refs(transport, branch);
    }

    // ブランチを追加・削除したときに替わる応答は、毎回状態を読んで返す
    fn register_refs(&self, transport: &RouteTransport, branch: &str) {
        // 書き込みを受け付けている間は、branchを進める応答も足す
        if self.writes_on.load(Ordering::SeqCst) {
            self.register_patch(transport, branch);
            self.register_delete(transport, branch);
        }
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

    // 書き込みの通信(POST /git/blobs・trees・commits・refs、PATCH /git/refs/heads/{branch})を
    // 受けて状態を替える。以後の読み込みの応答は状態から作り直されるので、Push後の中身も読める。
    // 受けたリクエストは、transport.requests()で読める
    pub fn accept_writes(&self, transport: &Arc<RouteTransport>) {
        self.writes_on.store(true, Ordering::SeqCst);
        let url = self.url("blobs", &[], &[]);
        let repo = self.clone();
        let routes = Arc::clone(transport);
        let captured = url.clone();
        transport.on(
            Method::Post,
            &url,
            Box::new(move |request| repo.post_blob(&routes, request, &captured)),
        );
        let url = self.url("trees", &[], &[]);
        let repo = self.clone();
        let routes = Arc::clone(transport);
        let captured = url.clone();
        transport.on(
            Method::Post,
            &url,
            Box::new(move |request| repo.post_tree(&routes, request, &captured)),
        );
        let url = self.url("commits", &[], &[]);
        let repo = self.clone();
        let routes = Arc::clone(transport);
        let captured = url.clone();
        transport.on(
            Method::Post,
            &url,
            Box::new(move |request| repo.post_commit(&routes, request, &captured)),
        );
        let url = self.url("refs", &[], &[]);
        let repo = self.clone();
        let routes = Arc::clone(transport);
        let captured = url.clone();
        transport.on(
            Method::Post,
            &url,
            Box::new(move |request| repo.post_ref(&routes, request, &captured)),
        );
        // 今あるbranchのPATCHの応答も足す。後から作るbranchは、register_refsが足す
        for name in self
            .branches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .cloned()
            .collect::<Vec<_>>()
        {
            self.register_patch(transport, &name);
            self.register_delete(transport, &name);
        }
    }

    // 書き込みの通信のnth回目の応答の前に、fを呼ぶ
    pub fn before(
        &self,
        method: Method,
        url: &str,
        nth: usize,
        f: impl Fn() + Send + Sync + 'static,
    ) {
        self.hooks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(hook_key(method, url))
            .or_default()
            .insert(nth, Arc::new(f));
    }

    // branchの先頭のコミットのルートのツリーを下りて、blobのファイルを取り出す(パス -> mode・中身)。
    // Push後のGitHub上の中身の確認に使う
    pub fn tree_files(&self, branch: &str) -> BTreeMap<String, (String, Vec<u8>)> {
        let commit = self
            .branches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(branch)
            .cloned()
            .expect("branchはあるはず");
        let root = {
            let tree = self
                .commits
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&commit)
                .cloned()
                .expect("コミットはあるはず");
            self.trees
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&tree)
                .cloned()
                .expect("ツリーはあるはず")
        };
        let mut files = BTreeMap::new();
        collect_files(&root, "", &mut files);
        files
    }

    pub fn head(&self, branch: &str) -> String {
        self.branches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(branch)
            .cloned()
            .expect("branchはあるはず")
    }

    // branchの先頭のコミットのルートのツリーのsha
    pub fn head_tree(&self, branch: &str) -> String {
        let commit = self
            .branches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(branch)
            .cloned()
            .expect("branchはあるはず");
        self.commits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&commit)
            .cloned()
            .expect("コミットはあるはず")
    }

    // 書き込みの通信のn回目で、beforeで登録した関数を呼ぶ
    fn fire(&self, method: Method, url: &str) {
        let key = hook_key(method, url);
        let nth = {
            let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
            let count = counts.entry(key.clone()).or_insert(0);
            *count += 1;
            *count
        };
        let hook = self
            .hooks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .and_then(|hooks| hooks.get(&nth))
            .cloned();
        if let Some(hook) = hook {
            hook();
        }
    }

    fn post_blob(
        &self,
        transport: &RouteTransport,
        request: &Request,
        url: &str,
    ) -> Result<Response, GithubError> {
        self.fire(Method::Post, url);
        let value = json_of(request);
        assert_eq!(value["encoding"], "base64", "blobはbase64で送るはず");
        let content = b64_decode(value["content"].as_str().expect("blobの中身はあるはず"))
            .expect("base64の中身は解けるはず");
        let sha = blob_sha(&content);
        self.blobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(sha.clone(), content.clone());
        // 以後GET /git/blobs/{sha}でも読める
        let blobs = Arc::clone(&self.blobs);
        let url = self.url("blobs", &[sha.as_str()], &[]);
        let key = sha.clone();
        transport.on(
            Method::Get,
            &url,
            Box::new(move |_| {
                let blobs = blobs.lock().unwrap_or_else(PoisonError::into_inner);
                match blobs.get(&key) {
                    Some(content) => Ok(Response {
                        status: 200,
                        headers: Vec::new(),
                        body: content.clone(),
                    }),
                    None => Ok(not_found()),
                }
            }),
        );
        Ok(json_response(201, serde_json::json!({ "sha": sha })))
    }

    fn post_tree(
        &self,
        transport: &RouteTransport,
        request: &Request,
        url: &str,
    ) -> Result<Response, GithubError> {
        self.fire(Method::Post, url);
        let value = json_of(request);
        let base = value["base_tree"].as_str().expect("base_treeはあるはず");
        let Some(mut node) = self
            .trees
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(base)
            .cloned()
        else {
            return Ok(json_response(
                422,
                serde_json::json!({ "message": "invalid base_tree" }),
            ));
        };
        // base_treeにエントリを重ねる。shaが無いエントリは消す
        for entry in value["tree"].as_array().expect("treeはあるはず") {
            let path = entry["path"].as_str().expect("エントリにはpathがあるはず");
            // GitHubは消すエントリに"sha": nullを求め、typeも要る
            assert!(
                entry.get("type").is_some(),
                "エントリにはtypeがあるはず: {entry}"
            );
            let sha = entry
                .get("sha")
                .expect("エントリにはshaがあるはず(消すならnull)")
                .as_str();
            // GitHubは、無いパスを消すエントリも、知らないSHAを指すエントリも422で断る
            if sha.is_none() {
                if !remove_path(&mut node, path) {
                    return Ok(json_response(
                        422,
                        serde_json::json!({ "message": "path does not exist" }),
                    ));
                }
                continue;
            }
            let mode = entry["mode"].as_str().unwrap_or("100644").to_string();
            let shares = sha.unwrap().to_string();
            let leaf = match self
                .blobs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&shares)
            {
                Some(content) => Node::Blob {
                    mode,
                    content: content.clone(),
                },
                None => match self
                    .trees
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&shares)
                    .cloned()
                {
                    Some(tree) => Node::Dir(tree),
                    None => {
                        return Ok(json_response(
                            422,
                            serde_json::json!({ "message": "unknown sha" }),
                        ))
                    }
                },
            };
            insert_path(&mut node, path, leaf);
        }
        prune(&mut node);
        let (sha, _) = self.register_tree(transport, &node);
        // 以後GET /git/trees/{sha}でも読める(register_tree)
        Ok(json_response(201, serde_json::json!({ "sha": sha })))
    }

    fn post_commit(
        &self,
        transport: &RouteTransport,
        request: &Request,
        url: &str,
    ) -> Result<Response, GithubError> {
        self.fire(Method::Post, url);
        let value = json_of(request);
        let tree = value["tree"].as_str().expect("treeはあるはず").to_string();
        let message = value["message"].as_str().unwrap_or_default().to_string();
        let parents: Vec<String> = value["parents"]
            .as_array()
            .expect("parentsはあるはず")
            .iter()
            .map(|parent| parent.as_str().expect("parentsは文字列のはず").to_string())
            .collect();
        let sha = format!("commit-{}", self.counter.fetch_add(1, Ordering::SeqCst));
        self.commits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(sha.clone(), tree.clone());
        self.parents
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(sha.clone(), parents.clone());
        self.register_commit(transport, &sha);
        self.register_compares(transport);
        // 以後GET /git/commits/{sha}でも読める。PATCHの早送りの判定にも親を使う
        Ok(json_response(
            201,
            serde_json::json!({
                "sha": sha,
                "message": message,
                "tree": { "sha": tree },
                "parents": parents,
            }),
        ))
    }

    fn post_ref(
        &self,
        transport: &RouteTransport,
        request: &Request,
        url: &str,
    ) -> Result<Response, GithubError> {
        self.fire(Method::Post, url);
        let value = json_of(request);
        let reference = value["ref"].as_str().expect("refはあるはず");
        let branch = reference
            .strip_prefix("refs/heads/")
            .expect("refはrefs/heads/の形のはず")
            .to_string();
        let sha = value["sha"].as_str().expect("shaはあるはず").to_string();
        {
            let mut branches = self.branches.lock().unwrap_or_else(PoisonError::into_inner);
            if branches.contains_key(&branch) {
                return Ok(json_response(
                    422,
                    serde_json::json!({ "message": "Reference already exists" }),
                ));
            }
            branches.insert(branch.clone(), sha.clone());
        }
        self.register_refs(transport, &branch);
        Ok(json_response(201, ref_json(&branch, &sha)))
    }

    // PATCH /git/refs/heads/{branch}の応答。今の先頭が新しいコミットの親のときだけ受け入れる
    fn register_patch(&self, transport: &RouteTransport, branch: &str) {
        let url = self.url(
            "refs/heads",
            branch.split('/').collect::<Vec<_>>().as_slice(),
            &[],
        );
        let repo = self.clone();
        let name = branch.to_string();
        let key = url.clone();
        transport.on(
            Method::Patch,
            &url,
            Box::new(move |request| repo.patch(&name, &key, request)),
        );
    }

    fn patch(&self, branch: &str, url: &str, request: &Request) -> Result<Response, GithubError> {
        self.fire(Method::Patch, url);
        let Some(head) = self
            .branches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(branch)
            .cloned()
        else {
            return Ok(json_response(
                422,
                serde_json::json!({ "message": "Reference does not exist" }),
            ));
        };
        let value = json_of(request);
        assert_eq!(
            value["force"],
            serde_json::Value::Bool(false),
            "PATCHはforce:falseで送るはず"
        );
        let new = value["sha"].as_str().expect("shaはあるはず").to_string();
        let fast = self
            .parents
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&new)
            .is_some_and(|parents| parents.iter().any(|parent| parent == &head));
        if !fast {
            return Ok(json_response(
                422,
                serde_json::json!({ "message": "Update is not a fast forward" }),
            ));
        }
        self.branches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(branch.to_string(), new.clone());
        Ok(json_response(200, ref_json(branch, &new)))
    }

    // DELETE /git/refs/heads/{branch}の応答。ブランチがあれば消して204、無ければ422
    fn register_delete(&self, transport: &RouteTransport, branch: &str) {
        let url = self.url(
            "refs/heads",
            branch.split('/').collect::<Vec<_>>().as_slice(),
            &[],
        );
        let repo = self.clone();
        let name = branch.to_string();
        let key = url.clone();
        transport.on(
            Method::Delete,
            &url,
            Box::new(move |request| repo.delete(&name, &key, request)),
        );
    }

    fn delete(&self, branch: &str, url: &str, _request: &Request) -> Result<Response, GithubError> {
        self.fire(Method::Delete, url);
        let removed = self
            .branches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(branch);
        if removed.is_none() {
            return Ok(json_response(
                422,
                serde_json::json!({ "message": "Reference does not exist" }),
            ));
        }
        // 実際のGitHubと同じく、消したブランチのコミットの応答は残す
        Ok(Response {
            status: 204,
            headers: Vec::new(),
            body: Vec::new(),
        })
    }

    // APIのURLを組み立てる。pathはgit/の後ろの部分
    fn url(&self, path: &str, segments: &[&str], query: &[(&str, &str)]) -> String {
        let mut all = vec!["repos", self.owner.as_str(), self.name.as_str(), "git"];
        all.extend(path.split('/'));
        all.extend(segments.iter().copied());
        api_url(&all, query).expect("fakeのURLは組み立てられるはず")
    }
}

// 資格情報の保存先の偽物。fail_*を立てた操作は失敗する
#[derive(Default)]
pub struct FakeSecret {
    value: Mutex<Option<String>>,
    fail_get: AtomicBool,
    fail_set: AtomicBool,
    fail_delete: AtomicBool,
}

impl FakeSecret {
    pub fn holding(tokens: &TokenSet) -> Self {
        Self::holding_raw(&serde_json::to_string(tokens).unwrap())
    }

    pub fn holding_raw(value: &str) -> Self {
        Self {
            value: Mutex::new(Some(value.to_string())),
            ..Self::default()
        }
    }

    pub fn value(&self) -> Option<String> {
        self.value
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn fail_get(&self, on: bool) {
        self.fail_get.store(on, Ordering::SeqCst);
    }

    pub fn fail_set(&self, on: bool) {
        self.fail_set.store(on, Ordering::SeqCst);
    }

    pub fn fail_delete(&self, on: bool) {
        self.fail_delete.store(on, Ordering::SeqCst);
    }
}

impl SecretStore for FakeSecret {
    fn get(&self) -> Result<Option<String>, String> {
        if self.fail_get.load(Ordering::SeqCst) {
            return Err("get failed".to_string());
        }
        Ok(self.value())
    }

    fn set(&self, value: &str) -> Result<(), String> {
        if self.fail_set.load(Ordering::SeqCst) {
            return Err("set failed".to_string());
        }
        *self.value.lock().unwrap_or_else(PoisonError::into_inner) = Some(value.to_string());
        Ok(())
    }

    fn delete(&self) -> Result<(), String> {
        if self.fail_delete.load(Ordering::SeqCst) {
            return Err("delete failed".to_string());
        }
        *self.value.lock().unwrap_or_else(PoisonError::into_inner) = None;
        Ok(())
    }
}

// デスクトップの置き場所。資格情報の保存先は偽物で、token.jsonはdata_dirの下。openと違い、token.jsonを移さない
pub fn desktop_tokens(data_dir: &Path) -> (TokenStore, Arc<FakeSecret>) {
    desktop_tokens_with(data_dir, FakeSecret::default())
}

pub fn desktop_tokens_with(data_dir: &Path, secret: FakeSecret) -> (TokenStore, Arc<FakeSecret>) {
    let secret = Arc::new(secret);
    let store = TokenStore::with_secret(secret.clone(), token_path(data_dir));
    (store, secret)
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use base64::Engine as _;

    use super::*;
    use crate::github::http::api_request;

    const A_ONE: &[u8] = b"one\n";
    const A_OLD: &[u8] = b"old branch\n";
    const A_NEW: &[u8] = b"new branch\n";
    const B: &[u8] = b"two\n";
    const C: &[u8] = b"three\n";

    fn compare(transport: &RouteTransport, base: &str, head: &str) -> Response {
        let request = api_request(Method::Get, &compare_url(base, head), "TOKEN", None)
            .expect("compareのURLは組み立てられるはず");
        transport.send(&request).expect("compareの応答はあるはず")
    }

    // 応答のfilesを(パス, status)にする
    fn files_of(response: &Response) -> Vec<(String, String)> {
        let value: serde_json::Value =
            serde_json::from_slice(&response.body).expect("応答はJSONのはず");
        value["files"]
            .as_array()
            .expect("filesは配列のはず")
            .iter()
            .map(|file| {
                (
                    file["filename"]
                        .as_str()
                        .expect("filenameはあるはず")
                        .to_string(),
                    file["status"]
                        .as_str()
                        .expect("statusはあるはず")
                        .to_string(),
                )
            })
            .collect()
    }

    fn pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(path, status)| (path.to_string(), status.to_string()))
            .collect()
    }

    fn post(transport: &RouteTransport, url: &str, body: serde_json::Value) -> serde_json::Value {
        let request =
            api_request(Method::Post, url, "TOKEN", Some(body)).expect("URLは組み立てられるはず");
        let response = transport.send(&request).expect("書き込みの応答はあるはず");
        serde_json::from_slice(&response.body).expect("応答はJSONのはず")
    }

    #[test]
    fn compareは分岐点からheadまでの差分を返す() {
        let transport = RouteTransport::new();
        let repo = FakeRepo::new(&transport, "owner", "blog");
        // ベースを1つ進めてから切ったブランチと、その前に切ったブランチ
        let first = repo.branch(&transport, "main", &[("src/content/blog/a.md", A_ONE)]);
        let second = repo.branch_from(
            &transport,
            "main",
            &first,
            &[
                ("src/content/blog/a.md", A_ONE),
                ("src/content/blog/b.md", B),
            ],
        );
        let old_branch = repo.branch_from(
            &transport,
            "blog/old",
            &first,
            &[("src/content/blog/a.md", A_OLD)],
        );
        let new_branch = repo.branch_from(
            &transport,
            "blog/new",
            &second,
            &[
                ("src/content/blog/a.md", A_NEW),
                ("src/content/blog/b.md", B),
                ("src/content/blog/c.md", C),
            ],
        );

        // 前に切ったブランチは、共通の祖先(first)からの差分になる。今のベースのツリーと比べると
        // b.mdが消えたことも入ってしまう
        assert_eq!(
            files_of(&compare(&transport, &second, &old_branch)),
            pairs(&[("src/content/blog/a.md", "modified")])
        );
        // 後に切ったブランチは、secondからの差分になる
        assert_eq!(
            files_of(&compare(&transport, &second, &new_branch)),
            pairs(&[
                ("src/content/blog/a.md", "modified"),
                ("src/content/blog/c.md", "added"),
            ])
        );
        // 同じコミットなら差分は無い
        assert!(files_of(&compare(&transport, &second, &second)).is_empty());
    }

    #[test]
    fn compareは共通の祖先が無ければ404() {
        let transport = RouteTransport::new();
        let repo = FakeRepo::new(&transport, "owner", "blog");
        let main = repo.branch(&transport, "main", &[("src/content/blog/a.md", A_ONE)]);
        let other = repo.orphan_branch(&transport, "other", &[("src/content/blog/a.md", A_OLD)]);

        let response = compare(&transport, &main, &other);
        assert_eq!(response.status, 404);
    }

    #[test]
    fn compareは書き込みで作ったコミットの親もたどる() {
        let transport = Arc::new(RouteTransport::new());
        let repo = FakeRepo::new(&transport, "owner", "blog");
        let base = repo.branch(&transport, "main", &[("src/content/blog/a.md", A_ONE)]);
        let base_tree = repo.head_tree("main");
        repo.accept_writes(&transport);

        let blob = post(
            &transport,
            &url_of(&["repos", "owner", "blog", "git", "blobs"], &[]),
            serde_json::json!({
                "content": base64::engine::general_purpose::STANDARD.encode(A_NEW),
                "encoding": "base64",
            }),
        );
        let tree = post(
            &transport,
            &url_of(&["repos", "owner", "blog", "git", "trees"], &[]),
            serde_json::json!({
                "base_tree": base_tree,
                "tree": [{
                    "path": "src/content/blog/a.md",
                    "mode": "100644",
                    "type": "blob",
                    "sha": blob["sha"],
                }],
            }),
        );
        let commit = post(
            &transport,
            &url_of(&["repos", "owner", "blog", "git", "commits"], &[]),
            serde_json::json!({
                "message": "update",
                "tree": tree["sha"],
                "parents": [base],
            }),
        );
        let head = commit["sha"].as_str().expect("shaはあるはず");

        // 書き込みのコミットをheadにして、親(base)との差分が読める
        assert_eq!(
            files_of(&compare(&transport, &base, head)),
            pairs(&[("src/content/blog/a.md", "modified")])
        );
    }
}
