use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

// base64を解く(#65のテストの道具)。paddingの=は読み飛ばす
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

// base_treeにエントリを重ねてできる TreeNode 用の道具
// パスのファイルを外し、その上に空になったディレクトリがあれば消す
fn remove_path(root: &mut TreeNode, path: &str) {
    let mut components: Vec<&str> = path.split('/').collect();
    let last = components.pop().expect("パスが空");
    let mut node = root;
    for component in components {
        match node.children.get_mut(component) {
            Some(Node::Dir(dir)) => node = dir,
            _ => return,
        }
    }
    node.children.remove(last);
    prune(node);
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
    // 書き込みで作ったコミットの親(PATCHの早送りの判定に使う)。branch()で作ったコミットは入らない
    parents: Arc<Mutex<BTreeMap<String, Vec<String>>>>,
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
        // 書き込みを受け付けている間は、branchを進める応答も足す
        if self.writes_on.load(Ordering::SeqCst) {
            self.register_patch(transport, branch);
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

    // branchの先頭のコミットのsha
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
            if sha.is_none() {
                remove_path(&mut node, path);
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
                    None => Node::Other {
                        mode,
                        kind: "commit".to_string(),
                        sha: shares,
                    },
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

    // APIのURLを組み立てる。pathはgit/の後ろの部分
    fn url(&self, path: &str, segments: &[&str], query: &[(&str, &str)]) -> String {
        let mut all = vec!["repos", self.owner.as_str(), self.name.as_str(), "git"];
        all.extend(path.split('/'));
        all.extend(segments.iter().copied());
        api_url(&all, query).expect("fakeのURLは組み立てられるはず")
    }
}
