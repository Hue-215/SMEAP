//! ブログ記事のファイルを読み書きする保存層。Tauriには依存しない。

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fmt::Display;
use std::fs;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

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
    // クリップボードはLinuxでだけ読む(src/clipboard.rs)
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[error("クリップボードを読めませんでした: {reason}")]
    Clipboard { reason: String },
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

// 読み取りの警告。keyは関係する既知のkey、keptは元の行をextraに残したとき
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Warning {
    pub(crate) key: Option<String>,
    pub(crate) message: String,
    pub(crate) kept: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Article {
    pub slug: String,
    #[serde(flatten)]
    pub fields: ArticleFields,
    // 消費しなかったエントリをそのまま
    #[serde(skip)]
    pub(crate) extra: Vec<Entry>,
    pub warnings: Vec<Warning>,
}

// 保存のときは画面からslugを除いた残りを受け取る
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticleFields {
    pub title: Option<String>,
    pub description: Option<String>,
    pub pub_date: Option<String>,
    pub hero_image: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticleSummary {
    pub slug: String,
    // 読めないときは空にして出す
    pub title: String,
    pub pub_date: Option<String>,
    pub warning: bool,
}

// 記事に結び付かない失敗
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnreadableEntry {
    pub path: String,
    pub reason: String,
}

impl UnreadableEntry {
    fn new(path: &Path, error: impl Display) -> Self {
        Self {
            path: path.display().to_string(),
            reason: error.to_string(),
        }
    }
}

// slugの分かる、開けない記事
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrokenArticle {
    pub slug: String,
    pub path: String,
    pub reason: String,
}

impl BrokenArticle {
    fn new(slug: &str, path: &Path, reason: impl Display) -> Self {
        Self {
            slug: slug.to_string(),
            path: path.display().to_string(),
            reason: reason.to_string(),
        }
    }
}

// 一覧で読めなかったものは、一覧全体を失敗させずにbrokenとunreadableで返す
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticleList {
    pub articles: Vec<ArticleSummary>,
    pub broken: Vec<BrokenArticle>,
    pub unreadable: Vec<UnreadableEntry>,
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

// `%m`・`%d`は0埋めの無い`2026-5-1`も通すので、書き戻して元と同じかで確かめる
fn is_valid_date(date: &str) -> bool {
    NaiveDate::parse_from_str(date, DATE_FORMAT)
        .is_ok_and(|parsed| parsed.format(DATE_FORMAT).to_string() == date)
}

// 保存のときに通す検証。読めていない項目は確かめない
fn check_fields(fields: &ArticleFields) -> Result<(), StoreError> {
    if let Some(date) = &fields.pub_date {
        if !is_valid_date(date) {
            return Err(StoreError::InvalidDate { date: date.clone() });
        }
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
                    unreadable: Vec::new(),
                })
            }
            Err(error) => return Err(StoreError::io(&self.root, error)),
        };

        let mut articles = Vec::new();
        let mut broken = Vec::new();
        let mut unreadable = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    unreadable.push(UnreadableEntry::new(&self.root, error));
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    unreadable.push(UnreadableEntry::new(&entry.path(), error));
                    continue;
                }
            };
            if !file_type.is_dir() {
                continue;
            }
            // 記事でないフォルダ(正しいslugでない)は黙って飛ばす
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                unreadable.push(UnreadableEntry::new(
                    &entry.path(),
                    "フォルダ名がUTF-8ではありません",
                ));
                continue;
            };
            if validate_slug(name).is_err() {
                continue;
            }
            match self.read_article(name) {
                Ok(article) => articles.push(ArticleSummary {
                    slug: article.slug,
                    title: article.fields.title.unwrap_or_default(),
                    pub_date: article.fields.pub_date,
                    warning: !article.warnings.is_empty(),
                }),
                Err(StoreError::NotFound { .. }) => {
                    // 記事の一部が残っていれば、壊れた記事として開けなかったと出す
                    let mut remains = false;
                    for dir in [self.posts_dir(name), self.assets_dir(name)] {
                        match dir.try_exists() {
                            Ok(true) => remains = true,
                            Ok(false) => {}
                            Err(error) => unreadable.push(UnreadableEntry::new(&dir, error)),
                        }
                    }
                    if remains {
                        broken.push(BrokenArticle::new(
                            name,
                            &self.md_path(name),
                            "mdがありません",
                        ));
                    }
                }
                Err(StoreError::Io { path, reason }) => broken.push(BrokenArticle {
                    slug: name.to_string(),
                    path,
                    reason,
                }),
                Err(error) => broken.push(BrokenArticle::new(name, &self.md_path(name), error)),
            }
        }
        broken.sort_by(|a, b| a.slug.cmp(&b.slug));
        articles.sort_by(|a, b| {
            b.pub_date
                .cmp(&a.pub_date)
                .then_with(|| a.slug.cmp(&b.slug))
        });
        Ok(ArticleList {
            articles,
            broken,
            unreadable,
        })
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
                    title: Some(String::new()),
                    description: Some(String::new()),
                    pub_date: Some(today()),
                    ..Default::default()
                },
                warnings: Vec::new(),
                extra: Vec::new(),
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
        Ok(parse_md(slug, &content))
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
        let md = self.md_path(slug);
        fs::create_dir_all(&self.backup_dir)
            .map_err(|error| StoreError::io(&self.backup_dir, error))?;
        let backup = self.backup_path(slug);
        fs::copy(&md, &backup)
            .map_err(|error| StoreError::not_found_or_io(slug, &backup, error))?;
        // 画面から渡らない読めなかった部分は、読み直した元の行のままで書く。
        // 開いたときに読めなかった項目は保存の時点で消費されていることがあるので、
        // Noneの項目は読み直して読めた値で補う
        let original = self.read_article(slug)?;
        let fields = ArticleFields {
            title: fields.title.or(original.fields.title),
            description: fields.description.or(original.fields.description),
            pub_date: fields.pub_date.or(original.fields.pub_date),
            hero_image: fields.hero_image.or(original.fields.hero_image),
            body: fields.body,
        };
        check_fields(&fields)?;
        self.write_md(&Article {
            slug: slug.to_string(),
            fields,
            warnings: Vec::new(),
            extra: original.extra,
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

    // 読めないmdの記事も消せるよう、read_articleはしない
    pub fn delete_article(&self, slug: &str) -> Result<(), StoreError> {
        validate_slug(slug)?;
        let _guard = self.lock_guard();
        let slug_dir = self.root.join(slug);
        fs::remove_dir_all(&slug_dir)
            .map_err(|error| StoreError::not_found_or_io(slug, &slug_dir, error))?;
        // 保存をしたことのない記事にはバックアップが無い
        let backup = self.backup_path(slug);
        match fs::remove_file(&backup) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(StoreError::io(&backup, error)),
        }
        Ok(())
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

    // 書き出しはAndroidだけ(src/export.rs)
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub fn write_zip(&self, slug: &str, dest: &Path) -> Result<(), StoreError> {
        validate_slug(slug)?;
        let _guard = self.lock_guard();
        let md = self.md_path(slug);
        let content =
            fs::read(&md).map_err(|error| StoreError::not_found_or_io(slug, &md, error))?;
        let names = self.asset_names(slug)?;

        write_zip_files(slug, &content, &self.assets_dir(slug), &names, dest)
    }

    // `.`で始まる名前は書きかけの一時ファイル
    fn asset_names(&self, slug: &str) -> Result<Vec<OsString>, StoreError> {
        let dir = self.assets_dir(slug);
        let read = match fs::read_dir(&dir) {
            Ok(read) => read,
            // gitは空ディレクトリを持たないため、assetsの無い記事もある
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(StoreError::io(&dir, error)),
        };
        let mut names = Vec::new();
        for entry in read {
            let entry = entry.map_err(|error| StoreError::io(&dir, error))?;
            if !entry
                .file_type()
                .map_err(|error| StoreError::io(&dir, error))?
                .is_file()
            {
                continue;
            }
            let name = entry.file_name();
            if !name.as_encoded_bytes().starts_with(b".") {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }
}

// zipの日時は時差を持たないので、端末の時刻で書く。画像は圧縮済みなので縮めない
fn write_zip_files(
    slug: &str,
    content: &[u8],
    assets_dir: &Path,
    names: &[OsString],
    dest: &Path,
) -> Result<(), StoreError> {
    let to_dest = |error: zip::result::ZipError| StoreError::io(dest, error);
    let file = File::create(dest).map_err(|error| StoreError::io(dest, error))?;
    let mut zip = ZipWriter::new(BufWriter::new(file));
    let stored = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(
            zip::DateTime::try_from(Local::now().naive_local()).unwrap_or_default(),
        );
    zip.start_file(
        format!("{slug}/src/content/posts/{slug}.md"),
        stored.compression_method(CompressionMethod::Deflated),
    )
    .map_err(to_dest)?;
    zip.write_all(content)
        .map_err(|error| StoreError::io(dest, error))?;
    for name in names {
        let path = assets_dir.join(name);
        let mut image = File::open(&path).map_err(|error| StoreError::io(&path, error))?;
        let entry = format!("{slug}/src/assets/{slug}/{}", name.to_string_lossy());
        zip.start_file(entry, stored).map_err(to_dest)?;
        io::copy(&mut image, &mut zip).map_err(|error| StoreError::io(&path, error))?;
    }
    zip.finish()
        .map_err(to_dest)?
        .into_inner()
        .map_err(|error| StoreError::io(dest, error.into_error()))?;
    Ok(())
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
    let fields = &article.fields;
    let mut md = String::from("---\n");
    // Someで書いた既知のkey。extraから同じkeyのエントリを除くために覚える
    let mut written: Vec<&str> = Vec::new();
    if let Some(title) = &fields.title {
        md.push_str(&format!("title: {}\n", json_string(title)));
        written.push("title");
    }
    if let Some(description) = &fields.description {
        md.push_str(&format!("description: {}\n", json_string(description)));
        written.push("description");
    }
    if let Some(pub_date) = &fields.pub_date {
        md.push_str(&format!("pubDate: {pub_date}\n"));
        written.push("pubDate");
    }
    if let Some(hero_image) = &fields.hero_image {
        let path = format!("{}{hero_image}", assets_ref(&article.slug));
        md.push_str(&format!("heroImage: {}\n", json_string(&path)));
        written.push("heroImage");
    }
    let extra = article
        .extra
        .iter()
        .filter(|entry| {
            !(entry
                .key_value
                .as_ref()
                .is_some_and(|(key, _)| written.contains(&key.as_str())))
        })
        .flat_map(|entry| entry.lines.iter().map(String::as_str));
    for line in extra {
        md.push_str(line);
        md.push('\n');
    }
    md.push_str("---\n");
    md.push_str(&fields.body);
    md
}

// フロントマターを`---\n`で囲んでいるときだけ分ける。2行目以降で最初に現れる`---`だけの行を
// 閉じとし、その行の後ろが本文。閉じの行がファイル末尾で改行が無くてもよい
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let rest = content.strip_prefix("---\n")?;
    let mut offset = 0;
    loop {
        let line_end = rest[offset..].find('\n').map_or(rest.len(), |i| offset + i);
        if &rest[offset..line_end] == "---" {
            // 閉じの行の直前の改行はfrontmatterに含めない
            let frontmatter = if offset == 0 { "" } else { &rest[..offset - 1] };
            let body = rest.get(line_end + 1..).unwrap_or("");
            return Some((frontmatter, body));
        }
        if line_end == rest.len() {
            return None;
        }
        offset = line_end + 1;
    }
}

// フロントマターのエントリ。続きの行も含めて元の行を覚え、読めなかった分を書き戻すため
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    // `key: value`の行から始まるエントリだけがkeyと値を持つ
    key_value: Option<(String, String)>,
    lines: Vec<String>,
}

// 既知のkeyの並び。重複の警告がこの並びで出る
const KNOWN_KEYS: [&str; 4] = ["title", "description", "pubDate", "heroImage"];

const REQUIRED_KEYS: [&str; 3] = ["title", "description", "pubDate"];

fn frontmatter_entries(frontmatter: &str) -> Vec<Entry> {
    // 項目を1つも書かない保存(`---\n---\n`)のときは、空のエントリにしない
    if frontmatter.is_empty() {
        return Vec::new();
    }
    let mut entries: Vec<Entry> = Vec::new();
    for line in frontmatter.split('\n') {
        let first = line.chars().next();
        let is_key = first.is_some_and(|c| !c.is_whitespace() && c != '#' && c != '-');
        if is_key {
            // コロンの無い行はkeyの無いエントリにする
            entries.push(Entry {
                key_value: line
                    .split_once(':')
                    .map(|(key, value)| (key.trim().to_string(), value.trim().to_string())),
                lines: vec![line.to_string()],
            });
        } else {
            // 空白か`-`で始まる行は前のエントリの続きにする。空行・コメントと、前のエントリが無い続きの行は独立のエントリにする
            let continues = first.is_some_and(|c| c.is_whitespace() || c == '-');
            match (continues, entries.last_mut()) {
                (true, Some(last)) => last.lines.push(line.to_string()),
                _ => entries.push(Entry {
                    key_value: None,
                    lines: vec![line.to_string()],
                }),
            }
        }
    }
    entries
}

fn known_index(entry: &Entry) -> Option<usize> {
    let key = entry.key_value.as_ref()?.0.as_str();
    KNOWN_KEYS.iter().position(|known| *known == key)
}

// フロントマターの値を文字列として読む。`"`の形はJSONの文字列で書くのでJSONとして読む
fn read_string(value: &str) -> Option<String> {
    if value.starts_with('"') {
        return serde_json::from_str(value).ok();
    }
    // `'`で始まり`'`で終わるときだけシングルクォートの形とみなす
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return Some(value[1..value.len() - 1].replace("''", "'"));
    }
    // 上の形でないなら、単純な文字列の値だけを読める。コメントの`#`と他のYAMLの形の値は文字列として確定しない
    if value.is_empty()
        || value.starts_with(['|', '>', '[', '{', '&', '*', '!', '%', '@', '`'])
        || value.starts_with('#')
        || value
            .split_once('#')
            .is_some_and(|(before, _)| before.chars().last().is_some_and(char::is_whitespace))
        || matches!(
            value.to_ascii_lowercase().as_str(),
            "null" | "~" | "true" | "false"
        )
        || value.parse::<f64>().is_ok()
    {
        return None;
    }
    Some(value.to_string())
}

// 読める値だけ読み、読めなかった部分はextraに元の行を残す
fn parse_md(slug: &str, content: &str) -> Article {
    // CRLFのmdは置き換えて読む
    let content = if content.contains('\r') {
        Cow::Owned(content.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(content)
    };
    // BOMはエディタが(fallbackで)付けることがあるので外して読む
    let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
    match split_frontmatter(content) {
        Some((frontmatter, body)) => parse_frontmatter(slug, frontmatter, body),
        None => Article {
            slug: slug.to_string(),
            fields: ArticleFields {
                body: content.to_owned(),
                ..Default::default()
            },
            warnings: vec![Warning {
                key: None,
                message: NO_FRONTMATTER.to_string(),
                kept: false,
            }],
            extra: Vec::new(),
        },
    }
}

const NO_FRONTMATTER: &str = "フロントマターがありません。全体を本文として読みました";

fn parse_frontmatter(slug: &str, frontmatter: &str, body: &str) -> Article {
    let entries = frontmatter_entries(frontmatter);
    let mut fields = ArticleFields {
        body: body.to_string(),
        ..Default::default()
    };

    // 既知のkeyごとのエントリの数。複数あるkeyはどのエントリも使わない
    let mut counts = [0usize; KNOWN_KEYS.len()];
    for entry in &entries {
        if let Some(index) = known_index(entry) {
            counts[index] += 1;
        }
    }

    // 重複した既知のkeyの警告。KNOWN_KEYSの並びで出す
    let mut warnings: Vec<Warning> = KNOWN_KEYS
        .iter()
        .zip(counts)
        .filter(|(_, count)| *count > 1)
        .map(|(key, _)| Warning {
            key: Some((*key).to_string()),
            message: format!("{key}が重複しています"),
            kept: true,
        })
        .collect();

    let warn_unreadable = |key: &str, value: &str| Warning {
        key: Some(key.to_string()),
        message: format!("{key}を読めません: {value}"),
        kept: true,
    };

    // 読めた値のエントリだけをconsumedにし、読めなかった値は警告にしてextraに残す
    let mut consumed = vec![false; entries.len()];
    for (i, entry) in entries.iter().enumerate() {
        let Some(index) = known_index(entry) else {
            continue;
        };
        if counts[index] > 1 {
            continue;
        }
        let Some((key, value)) = entry.key_value.as_ref() else {
            continue;
        };
        let key = key.as_str();
        if entry.lines.len() > 1 {
            // 続きの行がある値は確定しない。警告には1行目の値だけを出す
            warnings.push(warn_unreadable(key, value));
            continue;
        }
        let parsed = match key {
            "title" | "description" => read_string(value),
            "pubDate" => read_string(value).filter(|date| is_valid_date(date)),
            "heroImage" => read_string(value)
                .and_then(|path| image_name_from_ref(slug, &path).map(str::to_string)),
            _ => None,
        };
        if let Some(parsed) = parsed {
            match key {
                "title" => fields.title = Some(parsed),
                "description" => fields.description = Some(parsed),
                "pubDate" => fields.pub_date = Some(parsed),
                "heroImage" => fields.hero_image = Some(parsed),
                _ => {}
            }
            consumed[i] = true;
        } else {
            warnings.push(warn_unreadable(key, value));
        }
    }

    // 必須のkeyが無い警告
    for key in REQUIRED_KEYS {
        let index = KNOWN_KEYS.iter().position(|known| *known == key).unwrap();
        if counts[index] == 0 {
            warnings.push(Warning {
                key: Some(key.to_string()),
                message: format!("{key}がありません"),
                kept: false,
            });
        }
    }

    let extra: Vec<Entry> = entries
        .into_iter()
        .zip(consumed)
        .filter(|(_, consumed)| !*consumed)
        .map(|(entry, _)| entry)
        .collect();
    Article {
        slug: slug.to_string(),
        fields,
        warnings,
        extra,
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Read;

    use tempfile::TempDir;

    fn setup() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let store = Store::new(tmp.path().join("posts"), tmp.path().join("backup"));
        (tmp, store)
    }

    fn input(title: &str, pub_date: &str) -> ArticleFields {
        ArticleFields {
            title: Some(title.to_string()),
            description: Some(String::new()),
            pub_date: Some(pub_date.to_string()),
            hero_image: None,
            body: String::new(),
        }
    }

    fn article_of(slug: &str, fields: &ArticleFields) -> Article {
        Article {
            slug: slug.to_string(),
            fields: fields.clone(),
            warnings: Vec::new(),
            extra: Vec::new(),
        }
    }

    // Article.extraのエントリの、元の行を並べたもの
    // 警告の文言だけをまとめて確かめる
    fn warn_messages(warnings: &[Warning]) -> Vec<&str> {
        warnings.iter().map(|w| w.message.as_str()).collect()
    }

    fn extra_md(article: &Article) -> String {
        article
            .extra
            .iter()
            .flat_map(|entry| entry.lines.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }

    // mdを直接書いて読む。フロントマターの読み方を確かめるのに使う
    fn md_article(store: &Store, slug: &str, content: &str) -> Article {
        let md = store.md_path(slug);
        fs::create_dir_all(md.parent().unwrap()).unwrap();
        fs::write(&md, content).unwrap();
        store.read_article(slug).unwrap()
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
        assert_eq!(created.fields.title.as_deref(), Some(""));
        assert_eq!(created.fields.description.as_deref(), Some(""));
        assert_eq!(created.fields.pub_date.as_deref(), Some(today().as_str()));
        assert_eq!(created.fields.hero_image, None);
        assert_eq!(created.fields.body, "");
        assert!(created.extra.is_empty());
        assert!(created.warnings.is_empty());

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
            title: Some("改行\nとコロン: と\"引用符\"".to_string()),
            description: Some("説明\n2行目: これも".to_string()),
            pub_date: Some("2026-02-14".to_string()),
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
        // 開いたときに読めていた項目は、画面からnullで来ても読み直した値で書かれる
        let read = store.read_article("roundtrip").unwrap();
        assert_eq!(
            read.fields,
            ArticleFields {
                hero_image: Some("hero.png".to_string()),
                ..original
            }
        );
        let md = fs::read_to_string(store.md_path("roundtrip")).unwrap();
        assert!(md.contains("heroImage: \"../../assets/roundtrip/hero.png\""));
        assert!(md.ends_with(&format!("---\n{}", no_hero.body)));

        // 元々heroImageの無い記事は、nullのまま保存してもヒーローの行は書かれない
        store.create_article("no-hero").unwrap();
        let fields = input("t", "2026-01-01");
        store.save_article("no-hero", fields.clone()).unwrap();
        assert_eq!(
            store.read_article("no-hero").unwrap(),
            article_of("no-hero", &fields)
        );
        let md = fs::read_to_string(store.md_path("no-hero")).unwrap();
        assert!(!md.contains("heroImage"));
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
        assert!(list.unreadable.is_empty());
        // postsの下にmdが無いslugのフォルダは、壊れた記事としてbrokenに入る
        assert_eq!(list.broken.len(), 1);
        assert_eq!(list.broken[0].slug, "no-md");
        assert_eq!(list.broken[0].reason, "mdがありません");
        assert_eq!(
            list.broken[0].path,
            store.md_path("no-md").display().to_string()
        );
    }

    #[test]
    fn rootが無ければ一覧は空() {
        let (_tmp, store) = setup();
        let list = store.list_articles().unwrap();
        assert!(list.articles.is_empty());
        assert!(list.broken.is_empty());
        assert!(list.unreadable.is_empty());
    }

    #[test]
    fn mdの無い記事フォルダは一覧のbrokenに入る() {
        let (_tmp, store) = setup();
        store.create_article("good").unwrap();
        store
            .save_article("good", input("G", "2026-01-01"))
            .unwrap();
        store.create_article("broken").unwrap();
        fs::remove_file(store.md_path("broken")).unwrap();

        let list = store.list_articles().unwrap();
        assert_eq!(slugs(&list.articles), ["good"]);
        assert_eq!(list.broken.len(), 1);
        assert_eq!(list.broken[0].slug, "broken");
        assert_eq!(list.broken[0].reason, "mdがありません");
    }

    #[test]
    fn 開けない記事はbrokenにして無関係なフォルダは入らない() {
        let (_tmp, store) = setup();
        store.create_article("good").unwrap();
        store
            .save_article("good", input("G", "2026-01-01"))
            .unwrap();

        // UTF-8でないmd
        store.create_article("bad-utf8").unwrap();
        fs::write(store.md_path("bad-utf8"), [0xffu8, 0xfe, b'x']).unwrap();
        // mdが無い記事フォルダ(assetsだけ)
        fs::create_dir_all(store.assets_dir("no-md2")).unwrap();

        // slugの形でないフォルダと、記事の一部を持たないフォルダは出さない
        fs::create_dir_all(store.root.join("unrelated")).unwrap();
        fs::create_dir_all(store.root.join("Not-a-slug")).unwrap();

        let list = store.list_articles().unwrap();
        assert_eq!(slugs(&list.articles), ["good"]);
        assert!(list.unreadable.is_empty());
        let broken: Vec<&str> = list.broken.iter().map(|b| b.slug.as_str()).collect();
        assert_eq!(broken, ["bad-utf8", "no-md2"]);
        // UTF-8の失敗はmdのパスとエラーの文字列で返る
        assert_eq!(
            list.broken[0].path,
            store.md_path("bad-utf8").display().to_string()
        );
        assert!(
            list.broken[0].reason.contains("UTF-8"),
            "{}",
            list.broken[0].reason
        );
        assert_eq!(
            list.broken[1].path,
            store.md_path("no-md2").display().to_string()
        );
        assert_eq!(list.broken[1].reason, "mdがありません");
    }

    #[test]
    fn 一覧は日付の無い記事を後ろに並べる() {
        let (_tmp, store) = setup();
        // どちらもpubDateを読めないmd
        for slug in ["z-stack", "a-stack"] {
            md_article(
                &store,
                slug,
                &format!("---\ntitle: \"{slug}\"\ndescription: \"\"\npubDate: todo\n---\n"),
            );
        }
        // 同じ日付ならslug順
        for slug in ["m-post", "n-post"] {
            store.create_article(slug).unwrap();
            store.save_article(slug, input("", "2026-01-01")).unwrap();
        }

        let list = store.list_articles().unwrap();
        assert_eq!(
            slugs(&list.articles),
            ["m-post", "n-post", "a-stack", "z-stack"]
        );
        // 日付の無い記事には警告がある
        for summary in list.articles.iter().skip(2) {
            assert!(summary.warning);
            assert_eq!(summary.pub_date, None);
            // タイトルは読めている
            assert_ne!(summary.title, "");
        }
        for summary in list.articles.iter().take(2) {
            assert!(!summary.warning);
            assert_eq!(summary.pub_date.as_deref(), Some("2026-01-01"));
        }
    }

    #[test]
    fn renameで場所と参照とバックアップが移る() {
        let (_tmp, store) = setup();
        store.create_article("old-slug").unwrap();
        let first = ArticleFields {
            title: Some("タイトル".to_string()),
            description: Some(String::new()),
            pub_date: Some("2026-04-01".to_string()),
            hero_image: Some("hero.png".to_string()),
            body: "![絵](../../assets/old-slug/pic.png)".to_string(),
        };
        store.save_article("old-slug", first.clone()).unwrap();
        let revised = ArticleFields {
            title: Some("改訂".to_string()),
            pub_date: Some("2026-04-02".to_string()),
            ..first
        };
        store.save_article("old-slug", revised).unwrap();
        let backup = fs::read_to_string(store.backup_path("old-slug")).unwrap();
        assert!(backup.contains("title: \"タイトル\""));

        let renamed = store.rename_article("old-slug", "new-slug").unwrap();
        assert_eq!(renamed.slug, "new-slug");
        assert_eq!(renamed.fields.title.as_deref(), Some("改訂"));
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
    fn 削除するとフォルダとバックアップが消える() {
        let (_tmp, store) = setup();
        store.create_article("del-post").unwrap();
        store
            .save_article("del-post", input("v2", "2026-07-01"))
            .unwrap();
        fs::write(store.assets_dir("del-post").join("01.png"), [1u8, 2, 3]).unwrap();
        assert!(store.backup_path("del-post").exists());

        store.delete_article("del-post").unwrap();
        assert!(!store.root.join("del-post").exists());
        assert!(!store.backup_path("del-post").exists());
    }

    #[test]
    fn 読めないmdの記事も消せる() {
        let (_tmp, store) = setup();
        store.create_article("bad-utf8").unwrap();
        fs::write(store.md_path("bad-utf8"), [0xffu8, 0xfe]).unwrap();
        store.delete_article("bad-utf8").unwrap();
        assert!(!store.root.join("bad-utf8").exists());

        // mdだけが無い記事も消せる
        store.create_article("no-md").unwrap();
        fs::remove_file(store.md_path("no-md")).unwrap();
        store.delete_article("no-md").unwrap();
        assert!(!store.root.join("no-md").exists());
    }

    #[test]
    fn 存在しない記事は消せない() {
        let (_tmp, store) = setup();
        assert!(matches!(
            store.delete_article("missing"),
            Err(StoreError::NotFound { slug }) if slug == "missing"
        ));
    }

    #[test]
    fn 不正なslugは消せない() {
        let (_tmp, store) = setup();
        for slug in ["", "../x", "A", "a_b", "a/b"] {
            assert!(matches!(
                store.delete_article(slug),
                Err(StoreError::InvalidSlug { .. })
            ));
        }
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
            bad_input.pub_date = Some(bad.to_string());
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

    // 各項目のOption化や警告の有無に注目して、項目の値をまとめて確かめる
    fn assert_fields(
        article: &Article,
        title: Option<&str>,
        description: Option<&str>,
        pub_date: Option<&str>,
        hero_image: Option<&str>,
    ) {
        assert_eq!(article.fields.title.as_deref(), title);
        assert_eq!(article.fields.description.as_deref(), description);
        assert_eq!(article.fields.pub_date.as_deref(), pub_date);
        assert_eq!(article.fields.hero_image.as_deref(), hero_image);
    }

    #[test]
    fn フロントマターの無い記事は本文だけ直して保存しても本文が変わらない() {
        let (_tmp, store) = setup();
        let article = md_article(&store, "no-fm", "本文です\n");
        store.save_article("no-fm", article.fields.clone()).unwrap();
        let read = store.read_article("no-fm").unwrap();
        assert_eq!(read.fields.body, "本文です\n");
        assert!(read.extra.is_empty());
        assert_eq!(
            warn_messages(&read.warnings),
            [
                "titleがありません",
                "descriptionがありません",
                "pubDateがありません"
            ]
        );
    }

    #[test]
    fn フロントマターが無いと全体を本文として読む() {
        let (_tmp, store) = setup();
        // 囲み自体が無い
        let article = md_article(&store, "no-fence", "title: \"t\"\n\n本文です\n2行目\n");
        assert_fields(&article, None, None, None, None);
        assert_eq!(article.fields.body, "title: \"t\"\n\n本文です\n2行目\n");
        assert!(article.extra.is_empty());
        assert_eq!(
            warn_messages(&article.warnings),
            ["フロントマターがありません。全体を本文として読みました"]
        );

        // 閉じが無い
        let article = md_article(
            &store,
            "no-end",
            "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-01-01\n",
        );
        assert_fields(&article, None, None, None, None);
        assert_eq!(
            article.fields.body,
            "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-01-01\n"
        );
        assert_eq!(
            warn_messages(&article.warnings),
            ["フロントマターがありません。全体を本文として読みました"]
        );

        // 末尾の`\n---`でも閉じたとみなす
        let article = md_article(
            &store,
            "no-last-newline",
            "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-01-01\n---",
        );
        assert_fields(&article, Some("t"), Some(""), Some("2026-01-01"), None);
        assert_eq!(article.fields.body, "");
        assert!(article.warnings.is_empty());
    }

    #[test]
    fn crlfのmdは改行を置き換えて読む() {
        let (_tmp, store) = setup();
        let article = md_article(
            &store,
            "crlf",
            "---\r\ntitle: \"t\"\r\ndescription: \"d\"\r\npubDate: 2026-01-01\r\n---\r\n\r\n本文\r\n",
        );
        assert_fields(&article, Some("t"), Some("d"), Some("2026-01-01"), None);
        assert_eq!(article.fields.body, "\n本文\n");
        assert!(article.extra.is_empty());
        assert!(article.warnings.is_empty());
    }

    #[test]
    fn 引用符の無いtitleもリストの形のtitleも読む() {
        let (_tmp, store) = setup();
        let article = md_article(
            &store,
            "plain-title",
            "---\ntitle: タイトル\ndescription: \"\"\npubDate: 2026-01-01\n---\n",
        );
        assert_fields(
            &article,
            Some("タイトル"),
            Some(""),
            Some("2026-01-01"),
            None,
        );
        assert!(article.warnings.is_empty());

        let article = md_article(
            &store,
            "single-title",
            "---\ntitle: 'a''b'\ndescription: \"\"\npubDate: 2026-01-01\n---\n",
        );
        assert_eq!(article.fields.title.as_deref(), Some("a'b"));
        assert!(article.warnings.is_empty());

        // `'`で始まり`'`で終わらない値は、そのままの文字列として読む
        let article = md_article(
            &store,
            "open-single",
            "---\ntitle: 'それはそう\ndescription: \"\"\npubDate: 2026-01-01\n---\n",
        );
        assert_eq!(article.fields.title.as_deref(), Some("'それはそう"));
        assert!(article.warnings.is_empty());
    }

    #[test]
    fn 未知のkeyと空行とコメントはextraに残る() {
        let (_tmp, store) = setup();
        let article = md_article(
            &store,
            "extra-lines",
            "---\ntitle: \"t\"\n\n# コメント\ndescription: \"d\"\npubDate: 2026-01-01\ntags:\n  - a\n  - b\nmc: 入力\n---\n",
        );
        assert_fields(&article, Some("t"), Some("d"), Some("2026-01-01"), None);
        assert_eq!(
            extra_md(&article),
            "\n# コメント\ntags:\n  - a\n  - b\nmc: 入力"
        );
        assert!(article.warnings.is_empty());
    }

    #[test]
    fn 重複した既知のkeyはどれも読まない() {
        let (_tmp, store) = setup();
        let article = md_article(
            &store,
            "dup",
            "---\ntitle: \"t\"\ntitle: \"t2\"\ndescription: \"\"\npubDate: 2026-01-01\n---\n",
        );
        assert_fields(&article, None, Some(""), Some("2026-01-01"), None);
        assert_eq!(extra_md(&article), "title: \"t\"\ntitle: \"t2\"");
        assert_eq!(warn_messages(&article.warnings), ["titleが重複しています"]);
    }

    #[test]
    fn 読めない値は項目にせずextraに残す() {
        let (_tmp, store) = setup();
        // 日付の並びが変わる
        let article = md_article(
            &store,
            "loose-date",
            "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-5-1\n---\n",
        );
        assert_fields(&article, Some("t"), Some(""), None, None);
        assert_eq!(extra_md(&article), "pubDate: 2026-5-1");
        assert_eq!(
            warn_messages(&article.warnings),
            ["pubDateを読めません: 2026-5-1"]
        );

        // JSONの文字列になっていない
        let article = md_article(
            &store,
            "bad-hero",
            "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-01-01\nheroImage: 01.png\n---\n",
        );
        assert_fields(&article, Some("t"), Some(""), Some("2026-01-01"), None);
        assert_eq!(extra_md(&article), "heroImage: 01.png");
        assert_eq!(
            warn_messages(&article.warnings),
            ["heroImageを読めません: 01.png"]
        );

        // 別の記事の下を指している
        let article = md_article(
            &store,
            "hero-other",
            "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-01-01\nheroImage: \"../../assets/another/x.png\"\n---\n",
        );
        assert_fields(&article, Some("t"), Some(""), Some("2026-01-01"), None);
        assert_eq!(
            extra_md(&article),
            "heroImage: \"../../assets/another/x.png\""
        );
        assert_eq!(
            warn_messages(&article.warnings),
            ["heroImageを読めません: \"../../assets/another/x.png\""]
        );
    }

    #[test]
    fn 続きの行のある既知のkeyは読めない() {
        let (_tmp, store) = setup();
        // 1行目に値が無く、次の行に続く
        let article = md_article(
            &store,
            "cont-title",
            "---\ntitle:\n  続きの行\ndescription: \"\"\npubDate: 2026-01-01\n---\n",
        );
        assert_fields(&article, None, Some(""), Some("2026-01-01"), None);
        assert_eq!(extra_md(&article), "title:\n  続きの行");
        assert_eq!(warn_messages(&article.warnings), ["titleを読めません: "]);
    }

    #[test]
    fn 無い既知のkeyの警告はtitleから出す() {
        let (_tmp, store) = setup();
        let article = md_article(
            &store,
            "almost-empty",
            "---\nheroImage: \"../../assets/almost-empty/hero.png\"\n---\n",
        );
        assert_fields(&article, None, None, None, Some("hero.png"));
        assert_eq!(
            warn_messages(&article.warnings),
            [
                "titleがありません",
                "descriptionがありません",
                "pubDateがありません"
            ]
        );
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
                "warnings": [],
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
                title: Some("タイトル".to_string()),
                description: Some("説明".to_string()),
                pub_date: Some("2026-01-01".to_string()),
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
        assert_eq!(read.fields.title.as_deref(), Some("F"));
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
    fn フロントマターの無いmdにもheroを取り込める() {
        let (_tmp, store) = setup();
        store.create_article("broken").unwrap();
        fs::write(store.md_path("broken"), "フロントマターの無いmd").unwrap();
        assert_eq!(
            store.import_hero_image("broken", &png()).unwrap(),
            "hero.png"
        );
        assert_eq!(
            fs::read(store.assets_dir("broken").join("hero.png")).unwrap(),
            png()
        );
        // 項目は読めていないまま、後から画面で入れる
        let read = store.read_article("broken").unwrap();
        assert_eq!(read.fields.hero_image.as_deref(), Some("hero.png"));
        assert_eq!(read.fields.title, None);
        assert_eq!(read.fields.body, "フロントマターの無いmd");
    }

    #[test]
    fn 読めない日付は残しつつ値を入れると書き直される() {
        let (_tmp, store) = setup();
        let content = "---\ntitle: \"t\"\ndescription: \"d\"\npubDate: 2026-5-1\n---\n本文\n";
        let article = md_article(&store, "keep", content);
        assert_eq!(article.fields.pub_date, None);
        assert_eq!(extra_md(&article), "pubDate: 2026-5-1");

        store
            .save_article(
                "keep",
                ArticleFields {
                    pub_date: None,
                    ..article.fields.clone()
                },
            )
            .unwrap();
        assert_eq!(fs::read_to_string(store.md_path("keep")).unwrap(), content);

        let article = store.read_article("keep").unwrap();
        assert_eq!(
            warn_messages(&article.warnings),
            ["pubDateを読めません: 2026-5-1"]
        );

        store
            .save_article(
                "keep",
                ArticleFields {
                    pub_date: Some("2026-05-01".to_string()),
                    ..article.fields
                },
            )
            .unwrap();
        assert_eq!(
            fs::read_to_string(store.md_path("keep")).unwrap(),
            "---\ntitle: \"t\"\ndescription: \"d\"\npubDate: 2026-05-01\n---\n本文\n"
        );
        assert!(store.read_article("keep").unwrap().warnings.is_empty());
    }

    #[test]
    fn 未知のkeyと複数行の値は保存の後も残る() {
        let (_tmp, store) = setup();
        let content =
            "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-01-01\ntags:\n  - a\n  - b\nnote: めも\n---\n本文\n";
        let article = md_article(&store, "keep-extras", content);
        assert_eq!(extra_md(&article), "tags:\n  - a\n  - b\nnote: めも");

        store.save_article("keep-extras", article.fields).unwrap();
        assert_eq!(
            fs::read_to_string(store.md_path("keep-extras")).unwrap(),
            content
        );
        let article = store.read_article("keep-extras").unwrap();
        assert_eq!(extra_md(&article), "tags:\n  - a\n  - b\nnote: めも");
        assert!(article.warnings.is_empty());
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

    fn zip_names(dest: &Path) -> Vec<String> {
        let zip = zip::ZipArchive::new(fs::File::open(dest).unwrap()).unwrap();
        zip.file_names().map(str::to_string).collect()
    }

    #[test]
    fn zipにmdとassetsの画像が名前順で入る() {
        let (_tmp, store) = setup();
        store.create_article("post").unwrap();
        store
            .save_article("post", input("タイトル", "2026-01-01"))
            .unwrap();
        store.import_body_image("post", &jpeg()).unwrap();
        store.import_body_image("post", &png()).unwrap();

        let dest = store.root.parent().unwrap().join("post.zip");
        store.write_zip("post", &dest).unwrap();
        assert_eq!(
            zip_names(&dest),
            [
                "post/src/content/posts/post.md".to_string(),
                "post/src/assets/post/01.jpg".to_string(),
                "post/src/assets/post/02.png".to_string(),
            ]
        );
        let mut zip = zip::ZipArchive::new(fs::File::open(&dest).unwrap()).unwrap();
        let mut body = String::new();
        zip.by_name("post/src/content/posts/post.md")
            .unwrap()
            .read_to_string(&mut body)
            .unwrap();
        assert_eq!(body, fs::read_to_string(store.md_path("post")).unwrap());
    }

    #[test]
    fn zipはassetsが無ければmdだけ() {
        let (_tmp, store) = setup();
        store.create_article("fresh").unwrap();
        fs::remove_dir_all(store.assets_dir("fresh")).unwrap();

        let dest = store.root.parent().unwrap().join("fresh.zip");
        store.write_zip("fresh", &dest).unwrap();
        assert_eq!(
            zip_names(&dest),
            ["fresh/src/content/posts/fresh.md".to_string()]
        );
    }

    #[test]
    fn zipに書きかけのファイルは入らない() {
        let (_tmp, store) = setup();
        store.create_article("post").unwrap();
        fs::write(store.assets_dir("post").join(".tmpabc"), b"tmp").unwrap();
        store.import_body_image("post", &png()).unwrap();

        let dest = store.root.parent().unwrap().join("post.zip");
        store.write_zip("post", &dest).unwrap();
        assert_eq!(
            zip_names(&dest),
            [
                "post/src/content/posts/post.md".to_string(),
                "post/src/assets/post/01.png".to_string(),
            ]
        );
    }

    #[test]
    fn zipの画像は元のファイルと同じ中身() {
        let (_tmp, store) = setup();
        store.create_article("post").unwrap();
        store.import_body_image("post", &png()).unwrap();

        let dest = store.root.parent().unwrap().join("post.zip");
        store.write_zip("post", &dest).unwrap();
        let mut zip = zip::ZipArchive::new(fs::File::open(&dest).unwrap()).unwrap();
        let mut image = Vec::new();
        zip.by_name("post/src/assets/post/01.png")
            .unwrap()
            .read_to_end(&mut image)
            .unwrap();
        assert_eq!(image, png());
    }

    #[test]
    fn zipにassetsの中のディレクトリは入らない() {
        let (_tmp, store) = setup();
        store.create_article("post").unwrap();
        fs::create_dir(store.assets_dir("post").join("sub")).unwrap();

        let dest = store.root.parent().unwrap().join("post.zip");
        store.write_zip("post", &dest).unwrap();
        assert_eq!(
            zip_names(&dest),
            ["post/src/content/posts/post.md".to_string()]
        );
    }

    #[test]
    fn 不正なslugはzipにできない() {
        let (_tmp, store) = setup();
        let dest = store.root.parent().unwrap().join("bad.zip");
        for slug in ["", "../x"] {
            assert!(matches!(
                store.write_zip(slug, &dest),
                Err(StoreError::InvalidSlug { .. })
            ));
        }
        assert!(!dest.exists());
    }

    #[test]
    fn 存在しない記事はzipにできない() {
        let (_tmp, store) = setup();
        let dest = store.root.parent().unwrap().join("missing.zip");
        assert!(matches!(
            store.write_zip("missing", &dest),
            Err(StoreError::NotFound { slug }) if slug == "missing"
        ));
        assert!(!dest.exists());
    }

    #[test]
    fn zipにバックアップは入らない() {
        let (_tmp, store) = setup();
        store.create_article("post").unwrap();
        store
            .save_article("post", input("v2", "2026-01-01"))
            .unwrap();
        store
            .save_article("post", input("v3", "2026-01-02"))
            .unwrap();
        assert!(store.backup_path("post").exists());

        let dest = store.root.parent().unwrap().join("post.zip");
        store.write_zip("post", &dest).unwrap();
        let names = zip_names(&dest);
        assert!(
            !names.iter().any(|name| name.contains("backup")),
            "{names:?}"
        );
    }
    #[test]
    fn 外で直した読めない項目はnullの保存でも残る() {
        let (_tmp, store) = setup();
        let article = md_article(
            &store,
            "late-fix",
            "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-5-1\n---\n本文\n",
        );
        assert_eq!(article.fields.pub_date, None);
        // 保存までに外のエディタがmdを読める値に直す
        let fixed = "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-01-01\n---\n本文\n";
        fs::write(store.md_path("late-fix"), fixed).unwrap();
        store.save_article("late-fix", article.fields).unwrap();
        assert_eq!(
            fs::read_to_string(store.md_path("late-fix")).unwrap(),
            fixed
        );
        assert!(store.read_article("late-fix").unwrap().warnings.is_empty());
    }

    #[test]
    fn 重複したtitleの保存() {
        let (_tmp, store) = setup();
        let content =
            "---\ntitle: \"t\"\ntitle: \"t2\"\ndescription: \"\"\npubDate: 2026-01-01\n---\n本文\n";
        let article = md_article(&store, "dup-title", content);
        assert_eq!(warn_messages(&article.warnings), ["titleが重複しています"]);

        // Noneのままの保存は2行とも残す。読めなかった項目は書かないので、書かれる順はextraの並びになる
        store
            .save_article("dup-title", article.fields.clone())
            .unwrap();
        assert_eq!(
            fs::read_to_string(store.md_path("dup-title")).unwrap(),
            "---\ndescription: \"\"\npubDate: 2026-01-01\ntitle: \"t\"\ntitle: \"t2\"\n---\n本文\n"
        );

        // Someを入れた保存はtitleの行を1行にまとめる
        store
            .save_article(
                "dup-title",
                ArticleFields {
                    title: Some("t3".to_string()),
                    ..article.fields
                },
            )
            .unwrap();
        let md = fs::read_to_string(store.md_path("dup-title")).unwrap();
        assert_eq!(md.matches("title:").count(), 1);
        assert!(md.contains("title: \"t3\""));
        assert!(store.read_article("dup-title").unwrap().warnings.is_empty());
    }

    #[test]
    fn 読めない値のサムネイルをextraに残す記事の取り込み() {
        let (_tmp, store) = setup();
        md_article(
            &store,
            "hero",
            "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-01-01\nheroImage: 01.png\n---\n本文\n",
        );
        assert_eq!(store.import_hero_image("hero", &png()).unwrap(), "hero.png");
        let md = fs::read_to_string(store.md_path("hero")).unwrap();
        assert_eq!(md.matches("heroImage").count(), 1);
        assert!(md.contains("heroImage: \"../../assets/hero/hero.png\""));
    }

    #[test]
    fn renameしてもextraは残る() {
        let (_tmp, store) = setup();
        md_article(
            &store,
            "old",
            "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-01-01\ntags:\n  - a\nnote: こういう値\n---\n本文\n",
        );
        let renamed = store.rename_article("old", "new").unwrap();
        assert_eq!(extra_md(&renamed), "tags:\n  - a\nnote: こういう値");
        let read = store.read_article("new").unwrap();
        assert_eq!(extra_md(&read), "tags:\n  - a\nnote: こういう値");

        // 読めない値も残る
        md_article(
            &store,
            "old2",
            "---\ntitle: \"t\"\ndescription: \"\"\npubDate: 2026-01-01\nheroImage: 01.png\n---\n本文\n",
        );
        store.rename_article("old2", "new2").unwrap();
        let read = store.read_article("new2").unwrap();
        assert_eq!(extra_md(&read), "heroImage: 01.png");
    }

    #[test]
    fn split_frontmatterの分け方() {
        // 空のフロントマター
        assert_eq!(split_frontmatter("---\n---\n本文\n"), Some(("", "本文\n")));
        // 本文の中の`---`の行は本文。最初の閉じで分かれる
        assert_eq!(
            split_frontmatter("---\ntitle: \"t\"\n---\n---\n本文\n"),
            Some(("title: \"t\"", "---\n本文\n"))
        );
        // `--- `と`----`は閉じにならない
        assert_eq!(split_frontmatter("---\ntitle: \"t\"\n--- \n本文\n"), None);
        assert_eq!(split_frontmatter("---\ntitle: \"t\"\n----\n本文\n"), None);
    }

    #[test]
    fn frontmatter_entriesのエントリの区切り() {
        // 前のエントリが無い続きの行は独立のエントリ
        let entries = frontmatter_entries("  続きの行\nkey: 値\n");
        assert_eq!(
            entries,
            vec![
                Entry {
                    key_value: None,
                    lines: vec!["  続きの行".to_string()],
                },
                Entry {
                    key_value: Some(("key".to_string(), "値".to_string())),
                    lines: vec!["key: 値".to_string()],
                },
                // 末尾の空行も独立のエントリ
                Entry {
                    key_value: None,
                    lines: vec![String::new()],
                },
            ]
        );
        // コロンの無い行はkeyの無いエントリ
        let entries = frontmatter_entries("key: 値\nこういう行\n");
        assert_eq!(
            entries,
            vec![
                Entry {
                    key_value: Some(("key".to_string(), "値".to_string())),
                    lines: vec!["key: 値".to_string()],
                },
                Entry {
                    key_value: None,
                    lines: vec!["こういう行".to_string()],
                },
                Entry {
                    key_value: None,
                    lines: vec![String::new()],
                },
            ]
        );
    }

    #[test]
    fn 引用符の無い色々な値は読めない() {
        let (_tmp, store) = setup();
        for value in [
            "",
            "# コメント",
            "値 # コメント",
            "|",
            ">",
            "[",
            "{",
            "&",
            "*",
            "!",
            "%",
            "@",
            "`",
            "null",
            "NULL",
            "Null",
            "~",
            "true",
            "True",
            "false",
            "FALSE",
            "2026",
            "1.5",
            "-5",
        ] {
            let content =
                format!("---\ntitle: {value}\ndescription: \"\"\npubDate: 2026-01-01\n---\n");
            let article = md_article(&store, "unreadable", &content);
            assert!(
                article.fields.title.is_none(),
                "title: {value} は読めないはず"
            );
            assert_eq!(
                warn_messages(&article.warnings),
                [format!("titleを読めません: {value}")]
            );
            assert_eq!(extra_md(&article), format!("title: {value}"));
        }
    }

    #[test]
    fn bomの付いたmdを読める() {
        let (_tmp, store) = setup();
        let article = md_article(
            &store,
            "bom",
            "\u{feff}---\ntitle: \"t\"\ndescription: \"d\"\npubDate: 2026-01-01\n---\n本文\n",
        );
        assert_eq!(article.fields.title.as_deref(), Some("t"));
        assert_eq!(article.fields.body, "本文\n");
        assert!(article.warnings.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn utf8でない名前のフォルダはunreadableに入る() {
        use std::os::unix::ffi::OsStrExt;
        let (_tmp, store) = setup();
        let bad_dir = store.root.join(std::ffi::OsStr::from_bytes(b"\xff\xfe"));
        fs::create_dir_all(&bad_dir).unwrap();

        let list = store.list_articles().unwrap();
        assert!(list.articles.is_empty());
        assert!(list.broken.is_empty());
        assert_eq!(
            list.unreadable,
            vec![UnreadableEntry {
                path: bad_dir.display().to_string(),
                reason: "フォルダ名がUTF-8ではありません".to_string(),
            }]
        );
    }

    #[test]
    fn 一覧と警告とnull項目のjsonの形() {
        let list = ArticleList {
            articles: vec![ArticleSummary {
                slug: "a".to_string(),
                title: String::new(),
                pub_date: None,
                warning: true,
            }],
            broken: vec![BrokenArticle::new("b", Path::new("/x/y"), "理由")],
            unreadable: vec![UnreadableEntry::new(Path::new("/x/z"), "理由2")],
        };
        assert_eq!(
            serde_json::to_value(&list).unwrap(),
            serde_json::json!({
                "articles": [{"slug": "a", "title": "", "pubDate": null, "warning": true}],
                "broken": [{"slug": "b", "path": "/x/y", "reason": "理由"}],
                "unreadable": [{"path": "/x/z", "reason": "理由2"}],
            })
        );

        let article = Article {
            slug: "a".to_string(),
            fields: ArticleFields::default(),
            extra: Vec::new(),
            warnings: vec![Warning {
                key: Some("title".to_string()),
                message: "titleを読めません: こういう値".to_string(),
                kept: true,
            }],
        };
        assert_eq!(
            serde_json::to_value(&article).unwrap(),
            serde_json::json!({
                "slug": "a",
                "title": null,
                "description": null,
                "pubDate": null,
                "heroImage": null,
                "body": "",
                "warnings": [
                    {"key": "title", "message": "titleを読めません: こういう値", "kept": true}
                ],
            })
        );
    }
}
