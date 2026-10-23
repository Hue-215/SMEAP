//! ブログ記事のファイルを読み書きする保存層。Tauriには依存しない。

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt::Display;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};

const DATE_FORMAT: &str = "%Y-%m-%d";

// 画面で種類ごとに文言を選べるよう、kindに種類を入れてJSONにする
#[derive(Debug, Serialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StoreError {
    #[error("slugが不正です: {slug}")]
    InvalidSlug { slug: String },
    #[error("記事が既にあります: {slug}")]
    AlreadyExists { slug: String },
    #[error("記事がありません: {slug}")]
    NotFound { slug: String },
    #[error("日付が不正です: {date}")]
    InvalidDate { date: String },
    #[error("heroImageが不正です: {name}")]
    InvalidHeroImage { name: String },
    #[error("画像の参照が不正です: {reference}")]
    InvalidImageReference { reference: String },
    #[error("画像がありません: {reference}")]
    ImageNotFound { reference: String },
    #[error("画像の形式に対応していません")]
    UnsupportedImage,
    #[error("呼び出し方が不正です: {reason}")]
    InvalidRequest { reason: String },
    #[error("クリップボードを読めませんでした: {reason}")]
    Clipboard { reason: String },
    #[error("記事の形式が違います: {slug}: {reason}")]
    Malformed { slug: String, reason: String },
    #[error("ファイルの操作に失敗しました: {path}: {reason}")]
    Io { path: String, reason: String },
}

impl StoreError {
    pub(crate) fn io(path: &Path, error: impl Display) -> Self {
        StoreError::Io {
            path: path.display().to_string(),
            reason: error.to_string(),
        }
    }

    // 記事のファイルが無いのは、記事が無いものとして扱う
    fn not_found_or_io(slug: &str, path: &Path, error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::NotFound {
            StoreError::NotFound {
                slug: slug.to_string(),
            }
        } else {
            StoreError::io(path, error)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Article {
    pub slug: String,
    #[serde(flatten)]
    pub fields: ArticleFields,
}

// 保存のときは画面からslugを除いた残りを受け取る
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticleFields {
    pub title: String,
    pub description: String,
    pub pub_date: String,
    pub hero_image: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticleSummary {
    pub slug: String,
    pub title: String,
    pub pub_date: String,
}

// 一覧で読めなかった記事は、一覧全体を失敗させずにbrokenで返す
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticleList {
    pub articles: Vec<ArticleSummary>,
    pub broken: Vec<StoreError>,
}

pub fn validate_slug(slug: &str) -> Result<(), StoreError> {
    let valid = !slug.is_empty()
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if valid {
        Ok(())
    } else {
        Err(StoreError::InvalidSlug {
            slug: slug.to_string(),
        })
    }
}

fn today() -> String {
    Local::now().date_naive().format(DATE_FORMAT).to_string()
}

// 保存と読み込みの両方で通す検証
fn check_fields(fields: &ArticleFields) -> Result<(), StoreError> {
    // `%m`・`%d`は0埋めの無い`2026-5-1`も通すので、書き戻して元と同じかで確かめる
    let date_ok = NaiveDate::parse_from_str(&fields.pub_date, DATE_FORMAT)
        .is_ok_and(|date| date.format(DATE_FORMAT).to_string() == fields.pub_date);
    if !date_ok {
        return Err(StoreError::InvalidDate {
            date: fields.pub_date.clone(),
        });
    }
    if let Some(name) = &fields.hero_image {
        if !is_filename(name) {
            return Err(StoreError::InvalidHeroImage { name: name.clone() });
        }
    }
    Ok(())
}

// 本文とheroImageから画像を指すときの、記事ファイルからの相対パス
fn assets_ref(slug: &str) -> String {
    format!("../../assets/{slug}/")
}

fn image_name_from_ref<'a>(slug: &str, reference: &'a str) -> Option<&'a str> {
    reference
        .strip_prefix(&assets_ref(slug))
        .filter(|name| is_filename(name))
}

fn image_extension(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if data.starts_with(b"\xff\xd8\xff") {
        Some("jpg")
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some("gif")
    } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(&b"WEBP"[..]) {
        Some("webp")
    } else if data.get(4..8) == Some(&b"ftyp"[..])
        && matches!(data.get(8..12), Some(b"avif" | b"avis"))
    {
        Some("avif")
    } else {
        None
    }
}

// 連番として数える名前。拡張子は問わない。`parse`は`+1`も通すので、数字だけかを先に確かめる
fn image_number(name: &OsStr) -> Option<u32> {
    let stem = Path::new(name).file_stem()?.to_str()?;
    if !stem.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    stem.parse().ok()
}

fn next_image_number(dir: &Path) -> Result<u32, StoreError> {
    let mut max = 0;
    for entry in fs::read_dir(dir).map_err(|error| StoreError::io(dir, error))? {
        let entry = entry.map_err(|error| StoreError::io(dir, error))?;
        if let Some(number) = image_number(&entry.file_name()) {
            max = max.max(number);
        }
    }
    // u32::MAXの次は振れないので最大のまま返す。同じ名前があればpersist_noclobberで失敗する
    Ok(max.saturating_add(1))
}

pub struct Store {
    root: PathBuf,
    backup_dir: PathBuf,
    // コマンドはasyncで別スレッドから並んで呼ばれるので、書き換えの間は排他する
    lock: Mutex<()>,
}

impl Store {
    pub fn new(root: PathBuf, backup_dir: PathBuf) -> Self {
        Self {
            root,
            backup_dir,
            lock: Mutex::new(()),
        }
    }

    fn lock_guard(&self) -> MutexGuard<'_, ()> {
        match self.lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn posts_dir(&self, dir_slug: &str) -> PathBuf {
        self.root.join(dir_slug).join("src/content/posts")
    }

    fn md_path(&self, slug: &str) -> PathBuf {
        self.posts_dir(slug).join(format!("{slug}.md"))
    }

    // slugの変更の途中ではディレクトリと中身の名前が食い違うので、別々に受け取る
    fn assets_dir_in(&self, dir_slug: &str, slug: &str) -> PathBuf {
        self.root.join(dir_slug).join("src/assets").join(slug)
    }

    fn assets_dir(&self, slug: &str) -> PathBuf {
        self.assets_dir_in(slug, slug)
    }

    fn backup_path(&self, slug: &str) -> PathBuf {
        self.backup_dir.join(format!("{slug}.md"))
    }

    pub fn list_articles(&self) -> Result<ArticleList, StoreError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(ArticleList {
                    articles: Vec::new(),
                    broken: Vec::new(),
                })
            }
            Err(error) => return Err(StoreError::io(&self.root, error)),
        };

        let mut articles = Vec::new();
        let mut broken = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    broken.push(StoreError::io(&self.root, error));
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    broken.push(StoreError::io(&entry.path(), error));
                    continue;
                }
            };
            if !file_type.is_dir() {
                continue;
            }
            // 記事でないフォルダ(正しいslugでない、mdが無い)は黙って飛ばす
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            match self.read_article(name) {
                Ok(article) => articles.push(ArticleSummary {
                    slug: article.slug,
                    title: article.fields.title,
                    pub_date: article.fields.pub_date,
                }),
                Err(StoreError::InvalidSlug { .. } | StoreError::NotFound { .. }) => {}
                Err(error) => broken.push(error),
            }
        }
        articles.sort_by(|a, b| {
            b.pub_date
                .cmp(&a.pub_date)
                .then_with(|| a.slug.cmp(&b.slug))
        });
        Ok(ArticleList { articles, broken })
    }

    pub fn create_article(&self, slug: &str) -> Result<Article, StoreError> {
        validate_slug(slug)?;
        let _guard = self.lock_guard();
        fs::create_dir_all(&self.root).map_err(|error| StoreError::io(&self.root, error))?;
        // 先にexists()で確かめると並んだ呼び出しと競合するので、作ってAlreadyExistsを見る
        let slug_dir = self.root.join(slug);
        fs::create_dir(&slug_dir).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                StoreError::AlreadyExists {
                    slug: slug.to_string(),
                }
            } else {
                StoreError::io(&slug_dir, error)
            }
        })?;

        let result = (|| -> Result<Article, StoreError> {
            for dir in [self.posts_dir(slug), self.assets_dir(slug)] {
                fs::create_dir_all(&dir).map_err(|error| StoreError::io(&dir, error))?;
            }
            let article = Article {
                slug: slug.to_string(),
                fields: ArticleFields {
                    title: String::new(),
                    description: String::new(),
                    pub_date: today(),
                    hero_image: None,
                    body: String::new(),
                },
            };
            self.write_md(&article)?;
            Ok(article)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&slug_dir);
        }
        result
    }

    pub fn read_article(&self, slug: &str) -> Result<Article, StoreError> {
        validate_slug(slug)?;
        let md = self.md_path(slug);
        let content = fs::read_to_string(&md)
            .map_err(|error| StoreError::not_found_or_io(slug, &md, error))?;
        parse_md(slug, &content)
    }

    // referenceは本文に書いた参照(`../../assets/{slug}/01.png`)
    pub fn read_image(&self, slug: &str, reference: &str) -> Result<Vec<u8>, StoreError> {
        validate_slug(slug)?;
        let name = image_name_from_ref(slug, reference).ok_or_else(|| {
            StoreError::InvalidImageReference {
                reference: reference.to_string(),
            }
        })?;
        let path = self.assets_dir(slug).join(name);
        fs::read(&path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                StoreError::ImageNotFound {
                    reference: reference.to_string(),
                }
            } else {
                StoreError::io(&path, error)
            }
        })
    }

    // 本文に入れる`![](...)`を返す
    pub fn import_body_image(&self, slug: &str, data: &[u8]) -> Result<String, StoreError> {
        let _guard = self.lock_guard();
        let (dir, ext) = self.prepare_import(slug, data)?;
        let name = format!("{:02}.{}", next_image_number(&dir)?, ext);
        let path = dir.join(&name);
        let tmp = write_temp(&dir, data)?;
        tmp.persist_noclobber(&path)
            .map_err(|error| StoreError::io(&path, error))?;
        Ok(format!("![]({}{name})", assets_ref(slug)))
    }

    // heroImageに入れるファイル名を返す。mdのheroImageもここで書き換える
    pub fn import_hero_image(&self, slug: &str, data: &[u8]) -> Result<String, StoreError> {
        let _guard = self.lock_guard();
        let (dir, ext) = self.prepare_import(slug, data)?;
        let name = format!("hero.{ext}");
        let mut article = self.read_article(slug)?;
        let old = article.fields.hero_image.replace(name.clone());
        let path = dir.join(&name);
        let tmp = write_temp(&dir, data)?;
        tmp.persist(&path)
            .map_err(|error| StoreError::io(&path, error))?;
        if let Err(error) = self.write_md(&article) {
            // mdは前のheroImageを指したままなので、参照されない画像を残さない
            if old.as_deref() != Some(name.as_str()) {
                let _ = fs::remove_file(&path);
            }
            return Err(error);
        }
        if let Some(old) = old.filter(|old| {
            // 本文が参照していれば消さない
            old.as_str() != name
                && !article
                    .fields
                    .body
                    .contains(&format!("{}{old}", assets_ref(slug)))
        }) {
            // 取り込みは済んでいるので、消せなくても失敗にしない
            let old_path = dir.join(&old);
            if let Err(error) = fs::remove_file(&old_path) {
                if error.kind() != io::ErrorKind::NotFound {
                    eprintln!(
                        "前のheroImageを消せませんでした: {}",
                        StoreError::io(&old_path, error)
                    );
                }
            }
        }
        Ok(name)
    }

    fn prepare_import(
        &self,
        slug: &str,
        data: &[u8],
    ) -> Result<(PathBuf, &'static str), StoreError> {
        validate_slug(slug)?;
        let ext = image_extension(data).ok_or(StoreError::UnsupportedImage)?;
        let md = self.md_path(slug);
        fs::metadata(&md).map_err(|error| StoreError::not_found_or_io(slug, &md, error))?;
        // gitは空ディレクトリを持たないため
        let dir = self.assets_dir(slug);
        fs::create_dir_all(&dir).map_err(|error| StoreError::io(&dir, error))?;
        Ok((dir, ext))
    }

    pub fn save_article(&self, slug: &str, fields: ArticleFields) -> Result<(), StoreError> {
        validate_slug(slug)?;
        let _guard = self.lock_guard();
        check_fields(&fields)?;
        let md = self.md_path(slug);
        fs::create_dir_all(&self.backup_dir)
            .map_err(|error| StoreError::io(&self.backup_dir, error))?;
        let backup = self.backup_path(slug);
        fs::copy(&md, &backup)
            .map_err(|error| StoreError::not_found_or_io(slug, &backup, error))?;
        self.write_md(&Article {
            slug: slug.to_string(),
            fields,
        })
    }

    pub fn rename_article(&self, old: &str, new: &str) -> Result<Article, StoreError> {
        validate_slug(new)?;
        let _guard = self.lock_guard();
        let mut article = self.read_article(old)?;
        if old == new {
            return Ok(article);
        }
        let dest = self.root.join(new);
        match dest.try_exists() {
            Ok(true) => {
                return Err(StoreError::AlreadyExists {
                    slug: new.to_string(),
                })
            }
            Ok(false) => {}
            Err(error) => return Err(StoreError::io(&dest, error)),
        }
        // heroImageはファイル名で持つので、書くときに新しいslugで組み立て直される
        article.slug = new.to_string();
        article.fields.body = article
            .fields
            .body
            .replace(&assets_ref(old), &assets_ref(new));

        // gitは空ディレクトリを持たないので、画像の無い記事でもrenameできるように作る
        let old_assets = self.assets_dir(old);
        fs::create_dir_all(&old_assets).map_err(|error| StoreError::io(&old_assets, error))?;

        let mut renames = vec![
            (self.root.join(old), self.root.join(new)),
            (
                self.posts_dir(new).join(format!("{old}.md")),
                self.md_path(new),
            ),
            (self.assets_dir_in(new, old), self.assets_dir(new)),
        ];
        let old_backup = self.backup_path(old);
        if old_backup
            .try_exists()
            .map_err(|error| StoreError::io(&old_backup, error))?
        {
            renames.push((old_backup, self.backup_path(new)));
        }

        rename_all(&renames)?;
        if let Err(error) = self.write_md(&article) {
            undo_renames(&renames);
            return Err(error);
        }
        Ok(article)
    }

    // 一時ファイルに書いてから置き換える。途中で落ちても既存のmdは壊れない
    fn write_md(&self, article: &Article) -> Result<(), StoreError> {
        let md = self.md_path(&article.slug);
        let dir = self.posts_dir(&article.slug);
        let tmp = write_temp(&dir, render_md(article).as_bytes())?;
        tmp.persist(&md)
            .map_err(|error| StoreError::io(&md, error))?;
        Ok(())
    }
}

// persistをrenameで済ませるため、dirには置き換え先と同じディレクトリを渡す。上書きするかは呼び出し側がpersistかpersist_noclobberかで決める
fn write_temp(dir: &Path, content: &[u8]) -> Result<tempfile::NamedTempFile, StoreError> {
    // NamedTempFileは0600で作るので、0644で作る
    #[cfg(unix)]
    let mut tmp = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o644))
        .tempfile_in(dir)
        .map_err(|error| StoreError::io(dir, error))?;
    #[cfg(not(unix))]
    let mut tmp =
        tempfile::NamedTempFile::new_in(dir).map_err(|error| StoreError::io(dir, error))?;
    tmp.write_all(content)
        .map_err(|error| StoreError::io(tmp.path(), error))?;
    tmp.as_file()
        .sync_all()
        .map_err(|error| StoreError::io(tmp.path(), error))?;
    Ok(tmp)
}

// 順にrenameする。どれかが失敗したら、済んだものを逆順に戻してから失敗を返す
fn rename_all(renames: &[(PathBuf, PathBuf)]) -> Result<(), StoreError> {
    for (i, (from, to)) in renames.iter().enumerate() {
        if let Err(error) = fs::rename(from, to) {
            undo_renames(&renames[..i]);
            return Err(StoreError::io(from, error));
        }
    }
    Ok(())
}

// 済んだrenameを逆順に戻す。戻しの失敗は無視する
fn undo_renames(renames: &[(PathBuf, PathBuf)]) {
    for (from, to) in renames.iter().rev() {
        let _ = fs::rename(to, from);
    }
}

// `..`を含む名前はまとめて弾く。`.`だけの名前も弾く
fn is_filename(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
}

// 文字列の値はJSONの文字列にして書く。YAMLのダブルクォート文字列として読める
fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("文字列をJSONにできる")
}

fn render_md(article: &Article) -> String {
    let ArticleFields {
        title,
        description,
        pub_date,
        hero_image,
        body,
    } = &article.fields;
    let mut md = format!(
        "---\ntitle: {}\ndescription: {}\npubDate: {pub_date}\n",
        json_string(title),
        json_string(description),
    );
    if let Some(hero_image) = hero_image {
        let path = format!("{}{hero_image}", assets_ref(&article.slug));
        md.push_str(&format!("heroImage: {}\n", json_string(&path)));
    }
    md.push_str("---\n");
    md.push_str(body);
    md
}

// このアプリが書いた形だけを読む。手書きのYAMLは想定しない
fn parse_md(slug: &str, content: &str) -> Result<Article, StoreError> {
    let malformed = |reason: String| StoreError::Malformed {
        slug: slug.to_string(),
        reason,
    };
    let (frontmatter, body) = content
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .ok_or_else(|| malformed("フロントマターを---で囲んでいない".to_string()))?;

    let mut fields = HashMap::new();
    for line in frontmatter.split('\n') {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| malformed(format!("key: valueになっていない行がある: {line}")))?;
        if fields.insert(key, value.trim()).is_some() {
            return Err(malformed(format!("keyが重複している: {key}")));
        }
    }
    let known_keys = ["title", "description", "pubDate", "heroImage"];
    for key in fields.keys() {
        if !known_keys.contains(key) {
            return Err(malformed(format!("知らないkeyがある: {key}")));
        }
    }
    let field = |key: &str| {
        fields
            .get(key)
            .copied()
            .ok_or_else(|| malformed(format!("{key}がない")))
    };
    let string = |key: &str, value: &str| -> Result<String, StoreError> {
        serde_json::from_str(value)
            .map_err(|error| malformed(format!("{key}の値を文字列として読めない: {error}")))
    };

    let hero_image = match fields.get("heroImage") {
        Some(value) => {
            let path = string("heroImage", value)?;
            let name = image_name_from_ref(slug, &path).ok_or_else(|| {
                malformed(format!("heroImageが{}の下を指していない", assets_ref(slug)))
            })?;
            Some(name.to_string())
        }
        None => None,
    };

    let article_fields = ArticleFields {
        title: string("title", field("title")?)?,
        description: string("description", field("description")?)?,
        pub_date: field("pubDate")?.to_string(),
        hero_image,
        body: body.to_string(),
    };
    check_fields(&article_fields).map_err(|error| malformed(error.to_string()))?;

    Ok(Article {
        slug: slug.to_string(),
        fields: article_fields,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn setup() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let store = Store::new(tmp.path().join("posts"), tmp.path().join("backup"));
        (tmp, store)
    }

    fn input(title: &str, pub_date: &str) -> ArticleFields {
        ArticleFields {
            title: title.to_string(),
            description: String::new(),
            pub_date: pub_date.to_string(),
            hero_image: None,
            body: String::new(),
        }
    }

    fn article_of(slug: &str, fields: &ArticleFields) -> Article {
        Article {
            slug: slug.to_string(),
            fields: fields.clone(),
        }
    }

    fn slugs(articles: &[ArticleSummary]) -> Vec<&str> {
        articles.iter().map(|a| a.slug.as_str()).collect()
    }

    #[test]
    fn slugの検証() {
        for ok in ["a", "hello-world", "2026-01", "post-2"] {
            assert!(validate_slug(ok).is_ok(), "{ok} は妥当なはず");
        }
        for ng in ["", "Hello", "にほんご", "a b", "a/b", "a_b", "a.b"] {
            assert!(
                matches!(validate_slug(ng), Err(StoreError::InvalidSlug { .. })),
                "{ng} は不正なはず"
            );
        }
    }

    #[test]
    fn 作成直後の既定値() {
        let (_tmp, store) = setup();
        let created = store.create_article("first-post").unwrap();
        assert_eq!(created.slug, "first-post");
        assert_eq!(created.fields.title, "");
        assert_eq!(created.fields.description, "");
        assert_eq!(created.fields.pub_date, today());
        assert_eq!(created.fields.hero_image, None);
        assert_eq!(created.fields.body, "");

        let read = store.read_article("first-post").unwrap();
        assert_eq!(created, read);

        assert!(store.md_path("first-post").exists());
        assert!(store.assets_dir("first-post").is_dir());
    }

    #[test]
    fn 保存と読み込みの往復() {
        let (_tmp, store) = setup();
        store.create_article("roundtrip").unwrap();
        let original = ArticleFields {
            title: "改行\nとコロン: と\"引用符\"".to_string(),
            description: "説明\n2行目: これも".to_string(),
            pub_date: "2026-02-14".to_string(),
            hero_image: Some("hero.png".to_string()),
            body: "本文1行目\n2行目\n\n最終行".to_string(),
        };
        store.save_article("roundtrip", original.clone()).unwrap();
        assert_eq!(
            store.read_article("roundtrip").unwrap(),
            article_of("roundtrip", &original)
        );
        let md = fs::read_to_string(store.md_path("roundtrip")).unwrap();
        assert!(md.contains("heroImage: \"../../assets/roundtrip/hero.png\""));
        assert!(md.contains("pubDate: 2026-02-14"));

        let no_hero = ArticleFields {
            hero_image: None,
            ..original.clone()
        };
        store.save_article("roundtrip", no_hero.clone()).unwrap();
        assert_eq!(
            store.read_article("roundtrip").unwrap(),
            article_of("roundtrip", &no_hero)
        );
        let md = fs::read_to_string(store.md_path("roundtrip")).unwrap();
        assert!(!md.contains("heroImage"));
        assert!(md.ends_with(&format!("---\n{}", no_hero.body)));
    }

    #[test]
    fn 保存でバックアップに直前の内容が残る() {
        let (_tmp, store) = setup();
        store.create_article("backup-test").unwrap();
        store
            .save_article("backup-test", input("v2", "2026-03-01"))
            .unwrap();
        let backup = fs::read_to_string(store.backup_path("backup-test")).unwrap();
        assert!(backup.contains("title: \"\""));

        store
            .save_article("backup-test", input("v3", "2026-03-02"))
            .unwrap();
        let backup = fs::read_to_string(store.backup_path("backup-test")).unwrap();
        assert!(backup.contains("title: \"v2\""));
        assert!(!backup.contains("title: \"v3\""));
    }

    #[test]
    fn 重複作成は失敗する() {
        let (_tmp, store) = setup();
        store.create_article("dup").unwrap();
        assert!(matches!(
            store.create_article("dup"),
            Err(StoreError::AlreadyExists { .. })
        ));
    }

    #[test]
    fn 画像を読む() {
        let (_tmp, store) = setup();
        store.create_article("image").unwrap();
        fs::write(store.assets_dir("image").join("01.png"), [1u8, 2, 3]).unwrap();
        let reference = |name: &str| format!("../../assets/image/{name}");
        assert_eq!(
            store.read_image("image", &reference("01.png")).unwrap(),
            vec![1, 2, 3]
        );

        for bad in [
            "01.png".to_string(),
            "../../assets/other/01.png".to_string(),
            "https://example.com/01.png".to_string(),
            reference(""),
            reference("../up.png"),
            reference("a/b.png"),
            reference("a\\b.png"),
            reference("a..b.png"),
            reference("."),
        ] {
            assert!(
                matches!(
                    store.read_image("image", &bad),
                    Err(StoreError::InvalidImageReference { reference }) if reference == bad
                ),
                "{bad} は不正な参照のはず"
            );
        }
        for bad_slug in ["../x", "A", "a_b", "a/b"] {
            assert!(matches!(
                store.read_image(bad_slug, &reference("01.png")),
                Err(StoreError::InvalidSlug { .. })
            ));
        }
        assert!(matches!(
            store.read_image("image", &reference("02.png")),
            Err(StoreError::ImageNotFound { .. })
        ));
        // assetsのディレクトリがまだ無い記事
        store.create_article("no-assets").unwrap();
        assert!(matches!(
            store.read_image("no-assets", "../../assets/no-assets/01.png"),
            Err(StoreError::ImageNotFound { .. })
        ));
    }

    #[test]
    fn 存在しない記事は読めない() {
        let (_tmp, store) = setup();
        assert!(matches!(
            store.read_article("missing"),
            Err(StoreError::NotFound { .. })
        ));
        assert!(matches!(
            store.save_article("missing", input("t", "2026-01-01")),
            Err(StoreError::NotFound { .. })
        ));
    }

    #[test]
    fn 一覧は新しい順で不正なディレクトリを除く() {
        let (_tmp, store) = setup();
        for (slug, date) in [
            ("a-post", "2026-01-01"),
            ("b-post", "2026-01-01"),
            ("c-post", "2026-03-15"),
            ("d-post", "2025-08-09"),
        ] {
            store.create_article(slug).unwrap();
            store.save_article(slug, input("", date)).unwrap();
        }

        // 記事として扱いたくないもの
        fs::create_dir_all(store.root.join(".backup")).unwrap();
        fs::create_dir_all(store.root.join("Not-a-slug")).unwrap();
        fs::create_dir_all(store.root.join("no-md/src/content/posts")).unwrap();
        fs::write(store.root.join("loose-file.txt"), "").unwrap();

        let list = store.list_articles().unwrap();
        assert_eq!(
            slugs(&list.articles),
            ["c-post", "a-post", "b-post", "d-post"]
        );
        assert!(list.broken.is_empty());
    }

    #[test]
    fn rootが無ければ一覧は空() {
        let (_tmp, store) = setup();
        let list = store.list_articles().unwrap();
        assert!(list.articles.is_empty());
        assert!(list.broken.is_empty());
    }

    #[test]
    fn 壊れたmdのある記事は一覧のbrokenに入る() {
        let (_tmp, store) = setup();
        store.create_article("good").unwrap();
        store
            .save_article("good", input("G", "2026-01-01"))
            .unwrap();
        store.create_article("broken").unwrap();
        fs::write(store.md_path("broken"), "フロントマターの無いmd").unwrap();

        let list = store.list_articles().unwrap();
        assert_eq!(slugs(&list.articles), ["good"]);
        assert_eq!(list.broken.len(), 1);
    }

    #[test]
    fn renameで場所と参照とバックアップが移る() {
        let (_tmp, store) = setup();
        store.create_article("old-slug").unwrap();
        let first = ArticleFields {
            title: "タイトル".to_string(),
            description: String::new(),
            pub_date: "2026-04-01".to_string(),
            hero_image: Some("hero.png".to_string()),
            body: "![絵](../../assets/old-slug/pic.png)".to_string(),
        };
        store.save_article("old-slug", first.clone()).unwrap();
        let revised = ArticleFields {
            title: "改訂".to_string(),
            pub_date: "2026-04-02".to_string(),
            ..first
        };
        store.save_article("old-slug", revised).unwrap();
        let backup = fs::read_to_string(store.backup_path("old-slug")).unwrap();
        assert!(backup.contains("title: \"タイトル\""));

        let renamed = store.rename_article("old-slug", "new-slug").unwrap();
        assert_eq!(renamed.slug, "new-slug");
        assert_eq!(renamed.fields.title, "改訂");
        assert_eq!(renamed.fields.hero_image.as_deref(), Some("hero.png"));

        assert!(!store.root.join("old-slug").exists());
        assert!(store.md_path("new-slug").exists());
        assert!(store.assets_dir("new-slug").is_dir());

        let read = store.read_article("new-slug").unwrap();
        assert!(read.fields.body.contains("../../assets/new-slug/pic.png"));
        assert!(!read.fields.body.contains("../../assets/old-slug/"));

        let md = fs::read_to_string(store.md_path("new-slug")).unwrap();
        assert!(md.contains("heroImage: \"../../assets/new-slug/hero.png\""));

        assert!(store.backup_path("new-slug").exists());
        assert!(!store.backup_path("old-slug").exists());
    }

    #[test]
    fn renameで同じslugなら読んで返す() {
        let (_tmp, store) = setup();
        store.create_article("same").unwrap();
        let article = store.rename_article("same", "same").unwrap();
        assert_eq!(article.slug, "same");
        assert!(store.md_path("same").exists());
    }

    #[test]
    fn rename先に記事があると失敗する() {
        let (_tmp, store) = setup();
        store.create_article("first").unwrap();
        store.create_article("second").unwrap();
        assert!(matches!(
            store.rename_article("first", "second"),
            Err(StoreError::AlreadyExists { slug }) if slug == "second"
        ));
    }

    #[test]
    fn 不正な日付とヒーロー画像は保存できない() {
        let (_tmp, store) = setup();
        store.create_article("checks").unwrap();
        let base = input("タイトル", "2026-05-01");

        for bad in ["2026/05/01", "not-a-date", "", "2026年5月1日", "2026-5-1"] {
            let mut bad_input = base.clone();
            bad_input.pub_date = bad.to_string();
            assert!(
                matches!(
                    store.save_article("checks", bad_input),
                    Err(StoreError::InvalidDate { date }) if date == bad
                ),
                "{bad} は不正な日付のはず"
            );
        }
        for bad in ["", "../up.png", "a/b.png", "a\\b.png", "a..b.png"] {
            let mut bad_input = base.clone();
            bad_input.hero_image = Some(bad.to_string());
            assert!(
                matches!(
                    store.save_article("checks", bad_input),
                    Err(StoreError::InvalidHeroImage { name }) if name == bad
                ),
                "{bad} は不正なheroImageのはず"
            );
        }
    }

    #[test]
    fn rootの外へ抜けるslugは弾く() {
        let (tmp, store) = setup();
        assert!(matches!(
            store.read_article("../outside"),
            Err(StoreError::InvalidSlug { .. })
        ));
        assert!(matches!(
            store.create_article("../x"),
            Err(StoreError::InvalidSlug { .. })
        ));
        assert!(matches!(
            store.rename_article("../x", "a"),
            Err(StoreError::InvalidSlug { .. })
        ));
        assert!(matches!(
            store.rename_article("a", "../x"),
            Err(StoreError::InvalidSlug { .. })
        ));
        let mut entries: Vec<String> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        assert!(entries.is_empty(), "tmpの下に増えた: {entries:?}");
    }

    #[test]
    fn parse_mdの異常系はmalformedになる() {
        let (_tmp, store) = setup();
        for (slug, content) in [
            (
                "no-fence",
                "title: \"t\"\n---\n本文\n",
            ),
            (
                "no-end",
                "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-01-01\n",
            ),
            (
                "no-colon",
                "---\ntitle: \"t\"\ndescription: \"\"\nこういう行\npubDate: 2026-01-01\n---\n",
            ),
            (
                "dup-key",
                "---\ntitle: \"t\"\ntitle: \"t2\"\ndescription: \"\"\npubDate: 2026-01-01\n---\n",
            ),
            ("no-title", "---\ndescription: \"\"\npubDate: 2026-01-01\n---\n"),
            ("no-desc", "---\ntitle: \"t\"\npubDate: 2026-01-01\n---\n"),
            ("no-date", "---\ntitle: \"t\"\ndescription: \"\"\n---\n"),
            (
                "non-json-title",
                "---\ntitle: hello\ndescription: \"\"\npubDate: 2026-01-01\n---\n",
            ),
            (
                "unknown-key",
                "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-01-01\nextra: 1\n---\n",
            ),
            (
                "hero-other",
                "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-01-01\nheroImage: \"../../assets/another/x.png\"\n---\n",
            ),
            (
                "hero-empty",
                "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-01-01\nheroImage: \"../../assets/hero-empty/\"\n---\n",
            ),
            (
                "hero-sub",
                "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-01-01\nheroImage: \"../../assets/hero-sub/sub/x.png\"\n---\n",
            ),
            (
                "loose-date",
                "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-5-1\n---\n",
            ),
        ] {
            let md = store.md_path(slug);
            fs::create_dir_all(md.parent().unwrap()).unwrap();
            fs::write(&md, content).unwrap();
            assert!(
                matches!(store.read_article(slug), Err(StoreError::Malformed { .. })),
                "{slug} はMalformedのはず"
            );
        }
    }

    #[test]
    fn jsonの形() {
        let value = serde_json::to_value(StoreError::NotFound {
            slug: "a".to_string(),
        })
        .unwrap();
        assert_eq!(value, serde_json::json!({"kind": "notFound", "slug": "a"}));
        // 画面はkindで文言を選ぶ(SlugDialog.tsx)
        for (error, kind) in [
            (
                StoreError::AlreadyExists {
                    slug: "a".to_string(),
                },
                "alreadyExists",
            ),
            (
                StoreError::InvalidSlug {
                    slug: "a".to_string(),
                },
                "invalidSlug",
            ),
        ] {
            assert_eq!(serde_json::to_value(error).unwrap()["kind"], kind);
        }

        let article = article_of("a", &input("タイトル", "2026-01-01"));
        let value = serde_json::to_value(&article).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "slug": "a",
                "title": "タイトル",
                "description": "",
                "pubDate": "2026-01-01",
                "heroImage": null,
                "body": "",
            })
        );

        let fields: ArticleFields = serde_json::from_value(serde_json::json!({
            "title": "タイトル",
            "description": "説明",
            "pubDate": "2026-01-01",
            "heroImage": null,
            "body": "本文",
        }))
        .unwrap();
        assert_eq!(
            fields,
            ArticleFields {
                title: "タイトル".to_string(),
                description: "説明".to_string(),
                pub_date: "2026-01-01".to_string(),
                hero_image: None,
                body: "本文".to_string(),
            }
        );
    }

    #[test]
    fn バックアップの無い記事のrename() {
        let (_tmp, store) = setup();
        store.create_article("fresh").unwrap();
        store.rename_article("fresh", "fresh2").unwrap();
        assert!(store.md_path("fresh2").exists());
        assert!(!store.backup_path("fresh2").exists());
    }

    #[test]
    fn assetsの無い記事のrename() {
        let (_tmp, store) = setup();
        store.create_article("no-assets").unwrap();
        fs::remove_dir_all(store.assets_dir("no-assets")).unwrap();
        store.rename_article("no-assets", "renamed").unwrap();
        assert!(store.assets_dir("renamed").is_dir());
    }

    #[test]
    fn renameの失敗は元のまま() {
        let (_tmp, store) = setup();
        assert!(matches!(
            store.rename_article("missing", "a"),
            Err(StoreError::NotFound { slug }) if slug == "missing"
        ));

        store.create_article("first").unwrap();
        store.create_article("second").unwrap();
        store
            .save_article("first", input("F", "2026-01-01"))
            .unwrap();
        assert!(matches!(
            store.rename_article("first", "second"),
            Err(StoreError::AlreadyExists { slug }) if slug == "second"
        ));
        let read = store.read_article("first").unwrap();
        assert_eq!(read.fields.title, "F");
    }

    #[test]
    fn renameしても別のslugへの参照は変わらない() {
        let (_tmp, store) = setup();
        store.create_article("a").unwrap();
        let mut fields = input("", "2026-01-01");
        fields.body = "![絵](../../assets/a-b/x.png)".to_string();
        store.save_article("a", fields).unwrap();

        store.rename_article("a", "b").unwrap();
        let read = store.read_article("b").unwrap();
        assert!(read.fields.body.contains("../../assets/a-b/x.png"));
    }

    #[test]
    #[cfg(unix)]
    fn 保存したmdのパーミッション() {
        let (_tmp, store) = setup();
        store.create_article("perm").unwrap();
        let mode = store
            .md_path("perm")
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o644);
    }

    // 形式判定に通る、先頭バイトだけのダミー
    fn png() -> Vec<u8> {
        b"\x89PNG\r\n\x1a\n".to_vec()
    }

    fn jpeg() -> Vec<u8> {
        b"\xff\xd8\xff\xe0".to_vec()
    }

    #[test]
    fn 連番が01から振られる() {
        let (_tmp, store) = setup();
        store.create_article("seq").unwrap();
        assert_eq!(
            store.import_body_image("seq", &png()).unwrap(),
            "![](../../assets/seq/01.png)"
        );
        assert_eq!(
            store.import_body_image("seq", &png()).unwrap(),
            "![](../../assets/seq/02.png)"
        );
        assert_eq!(
            fs::read(store.assets_dir("seq").join("01.png")).unwrap(),
            png()
        );
    }

    #[test]
    fn 連番は最大の次で空きを詰めない() {
        let (_tmp, store) = setup();
        store.create_article("gap").unwrap();
        let dir = store.assets_dir("gap");
        fs::write(dir.join("01.png"), b"old").unwrap();
        fs::write(dir.join("05.JPG"), b"old").unwrap();
        assert_eq!(
            store.import_body_image("gap", &png()).unwrap(),
            "![](../../assets/gap/06.png)"
        );
        assert_eq!(fs::read(dir.join("01.png")).unwrap(), b"old");
        assert_eq!(fs::read(dir.join("05.JPG")).unwrap(), b"old");
    }

    #[test]
    fn 連番は数字でない名前を数えない() {
        let (_tmp, store) = setup();
        store.create_article("count").unwrap();
        let dir = store.assets_dir("count");
        for name in ["hero.png", "notes.txt", ".png", "x.png"] {
            fs::write(dir.join(name), b"old").unwrap();
        }
        assert_eq!(
            store.import_body_image("count", &png()).unwrap(),
            "![](../../assets/count/01.png)"
        );
        // u32に入らない番号も数えない
        fs::write(dir.join("4294967296.png"), b"old").unwrap();
        assert_eq!(
            store.import_body_image("count", &png()).unwrap(),
            "![](../../assets/count/02.png)"
        );
    }

    #[test]
    fn 形式から拡張子が決まる() {
        let (_tmp, store) = setup();
        store.create_article("format").unwrap();
        for (data, ext) in [
            (&png()[..], "png"),
            (&jpeg()[..], "jpg"),
            (&b"GIF87a"[..], "gif"),
            (&b"GIF89a"[..], "gif"),
            (&b"RIFF\x24\x00\x00\x00WEBP"[..], "webp"),
            (&b"\x00\x00\x00\x20ftypavis"[..], "avif"),
            (&b"\x00\x00\x00\x20ftypavif"[..], "avif"),
        ] {
            let reference = store.import_body_image("format", data).unwrap();
            assert!(
                reference.ends_with(&format!(".{ext})")),
                "{reference} は {ext} のはず"
            );
        }

        for bad in [
            &b""[..],
            &b"MZ"[..],
            &b"RIFF\x24\x00"[..],
            &b"\x00\x00\x00\x20ftypmp42"[..],
        ] {
            assert!(
                matches!(
                    store.import_body_image("format", bad),
                    Err(StoreError::UnsupportedImage)
                ),
                "未対応の形式のはず"
            );
        }
    }

    #[test]
    fn heroは上書きしてmdのhero_imageが変わる() {
        let (_tmp, store) = setup();
        store.create_article("hero").unwrap();
        let dir = store.assets_dir("hero");
        assert_eq!(store.import_hero_image("hero", &png()).unwrap(), "hero.png");
        assert_eq!(fs::read(dir.join("hero.png")).unwrap(), png());
        let read = store.read_article("hero").unwrap();
        assert_eq!(read.fields.hero_image.as_deref(), Some("hero.png"));

        assert_eq!(
            store.import_hero_image("hero", &jpeg()).unwrap(),
            "hero.jpg"
        );
        assert!(!dir.join("hero.png").exists());
        assert_eq!(fs::read(dir.join("hero.jpg")).unwrap(), jpeg());
        let read = store.read_article("hero").unwrap();
        assert_eq!(read.fields.hero_image.as_deref(), Some("hero.jpg"));
    }

    #[test]
    fn heroは前のhero_imageのファイルを消す() {
        let (_tmp, store) = setup();
        store.create_article("post").unwrap();
        let mut fields = input("", "2026-01-01");
        fields.hero_image = Some("cover.png".to_string());
        store.save_article("post", fields).unwrap();
        let dir = store.assets_dir("post");
        fs::write(dir.join("cover.png"), b"cover").unwrap();
        fs::write(dir.join("01.png"), b"old").unwrap();

        assert_eq!(store.import_hero_image("post", &png()).unwrap(), "hero.png");
        assert!(!dir.join("cover.png").exists());
        // 前のheroImage以外は消さない
        assert_eq!(fs::read(dir.join("01.png")).unwrap(), b"old");
        let read = store.read_article("post").unwrap();
        assert_eq!(read.fields.hero_image.as_deref(), Some("hero.png"));

        // 前のファイルが既に無くても取り込める
        fs::remove_file(dir.join("hero.png")).unwrap();
        assert_eq!(
            store.import_hero_image("post", &jpeg()).unwrap(),
            "hero.jpg"
        );
    }

    #[test]
    fn heroは本文が参照している前のhero_imageを消さない() {
        let (_tmp, store) = setup();
        store.create_article("post").unwrap();
        let mut fields = input("", "2026-01-01");
        fields.hero_image = Some("01.png".to_string());
        fields.body = "![表紙](../../assets/post/01.png)".to_string();
        store.save_article("post", fields).unwrap();
        let dir = store.assets_dir("post");
        fs::write(dir.join("01.png"), b"old").unwrap();

        assert_eq!(store.import_hero_image("post", &png()).unwrap(), "hero.png");
        assert_eq!(fs::read(dir.join("01.png")).unwrap(), b"old");
        let read = store.read_article("post").unwrap();
        assert_eq!(read.fields.hero_image.as_deref(), Some("hero.png"));
    }

    #[test]
    fn heroを同じ形式で2回取り込んでも名前は変わらない() {
        let (_tmp, store) = setup();
        store.create_article("same-format").unwrap();
        let dir = store.assets_dir("same-format");
        store.import_hero_image("same-format", &png()).unwrap();
        let mut second = png();
        second.push(1);
        assert_eq!(
            store.import_hero_image("same-format", &second).unwrap(),
            "hero.png"
        );
        assert_eq!(fs::read(dir.join("hero.png")).unwrap(), second);
        let read = store.read_article("same-format").unwrap();
        assert_eq!(read.fields.hero_image.as_deref(), Some("hero.png"));
    }

    #[test]
    fn mdが壊れた記事へのhero取り込みは失敗する() {
        let (_tmp, store) = setup();
        store.create_article("broken").unwrap();
        fs::write(store.md_path("broken"), "フロントマターの無いmd").unwrap();
        assert!(matches!(
            store.import_hero_image("broken", &png()),
            Err(StoreError::Malformed { .. })
        ));
        assert!(fs::read_dir(store.assets_dir("broken"))
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn 取り込む記事が無ければ失敗する() {
        let (_tmp, store) = setup();
        assert!(matches!(
            store.import_body_image("missing", &png()),
            Err(StoreError::NotFound { slug }) if slug == "missing"
        ));
        assert!(matches!(
            store.import_hero_image("missing", &png()),
            Err(StoreError::NotFound { slug }) if slug == "missing"
        ));
        assert!(!store.assets_dir("missing").exists());
    }

    #[test]
    fn 不正なslugは取り込めない() {
        let (_tmp, store) = setup();
        for slug in ["", "../x", "A", "a_b", "a/b"] {
            assert!(matches!(
                store.import_body_image(slug, &png()),
                Err(StoreError::InvalidSlug { .. })
            ));
            assert!(matches!(
                store.import_hero_image(slug, &png()),
                Err(StoreError::InvalidSlug { .. })
            ));
        }
    }

    #[test]
    fn assetsが無くても取り込める() {
        let (_tmp, store) = setup();
        store.create_article("fresh").unwrap();
        fs::remove_dir_all(store.assets_dir("fresh")).unwrap();
        assert_eq!(
            store.import_body_image("fresh", &png()).unwrap(),
            "![](../../assets/fresh/01.png)"
        );
        assert_eq!(
            store.import_hero_image("fresh", &png()).unwrap(),
            "hero.png"
        );
        assert!(store.assets_dir("fresh").join("01.png").exists());
        assert!(store.assets_dir("fresh").join("hero.png").exists());
    }
}
