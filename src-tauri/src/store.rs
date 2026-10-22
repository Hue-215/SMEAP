//! ブログ記事のファイルを読み書きする保存層。Tauriには依存しない。

use std::collections::HashMap;
use std::fmt::Display;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

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
    #[error("記事の形式が違います: {slug}: {reason}")]
    Malformed { slug: String, reason: String },
    #[error("ファイルの操作に失敗しました: {path}: {reason}")]
    Io { path: String, reason: String },
}

impl StoreError {
    fn io(path: &Path, error: impl Display) -> Self {
        StoreError::Io {
            path: path.display().to_string(),
            reason: error.to_string(),
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

// 本文とheroImageから画像を指すときの、記事ファイルからの相対パス
fn assets_ref(slug: &str) -> String {
    format!("../../assets/{slug}/")
}

pub struct Store {
    root: PathBuf,
    backup_dir: PathBuf,
}

impl Store {
    pub fn new(root: PathBuf, backup_dir: PathBuf) -> Self {
        Self { root, backup_dir }
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

    pub fn list_articles(&self) -> Vec<ArticleSummary> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut articles: Vec<_> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
            // 正しいslugでないフォルダや読めない記事は、一覧全体を失敗させずに飛ばす
            .filter_map(|entry| self.read_article(entry.file_name().to_str()?).ok())
            .map(|article| ArticleSummary {
                slug: article.slug,
                title: article.fields.title,
                pub_date: article.fields.pub_date,
            })
            .collect();
        articles.sort_by(|a, b| {
            b.pub_date
                .cmp(&a.pub_date)
                .then_with(|| a.slug.cmp(&b.slug))
        });
        articles
    }

    pub fn create_article(&self, slug: &str) -> Result<Article, StoreError> {
        validate_slug(slug)?;
        if self.root.join(slug).exists() {
            return Err(StoreError::AlreadyExists {
                slug: slug.to_string(),
            });
        }
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
    }

    pub fn read_article(&self, slug: &str) -> Result<Article, StoreError> {
        validate_slug(slug)?;
        let md = self.md_path(slug);
        let content = fs::read_to_string(&md).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                StoreError::NotFound {
                    slug: slug.to_string(),
                }
            } else {
                StoreError::io(&md, error)
            }
        })?;
        parse_md(slug, &content)
    }

    pub fn save_article(&self, slug: &str, fields: ArticleFields) -> Result<(), StoreError> {
        validate_slug(slug)?;
        let md = self.md_path(slug);
        if !md.exists() {
            return Err(StoreError::NotFound {
                slug: slug.to_string(),
            });
        }
        // `%m`・`%d`は0埋めの無い`2026-5-1`も通すので、書き戻して元と同じかで確かめる
        let date_ok = NaiveDate::parse_from_str(&fields.pub_date, DATE_FORMAT)
            .is_ok_and(|date| date.format(DATE_FORMAT).to_string() == fields.pub_date);
        if !date_ok {
            return Err(StoreError::InvalidDate {
                date: fields.pub_date,
            });
        }
        if let Some(name) = &fields.hero_image {
            if !is_filename(name) {
                return Err(StoreError::InvalidHeroImage { name: name.clone() });
            }
        }
        fs::create_dir_all(&self.backup_dir)
            .map_err(|error| StoreError::io(&self.backup_dir, error))?;
        let backup = self.backup_path(slug);
        fs::copy(&md, &backup).map_err(|error| StoreError::io(&backup, error))?;
        self.write_md(&Article {
            slug: slug.to_string(),
            fields,
        })
    }

    pub fn rename_article(&self, old: &str, new: &str) -> Result<Article, StoreError> {
        validate_slug(new)?;
        let mut article = self.read_article(old)?;
        if old == new {
            return Ok(article);
        }
        if self.root.join(new).exists() {
            return Err(StoreError::AlreadyExists {
                slug: new.to_string(),
            });
        }
        // heroImageはファイル名で持つので、書くときに新しいslugで組み立て直される
        article.slug = new.to_string();
        article.fields.body = article
            .fields
            .body
            .replace(&assets_ref(old), &assets_ref(new));

        let renames = [
            (self.root.join(old), self.root.join(new)),
            (
                self.posts_dir(new).join(format!("{old}.md")),
                self.md_path(new),
            ),
            (self.assets_dir_in(new, old), self.assets_dir(new)),
        ];
        for (from, to) in renames {
            fs::rename(&from, &to).map_err(|error| StoreError::io(&from, error))?;
        }
        self.write_md(&article)?;

        let old_backup = self.backup_path(old);
        if old_backup.exists() {
            fs::rename(&old_backup, self.backup_path(new))
                .map_err(|error| StoreError::io(&old_backup, error))?;
        }
        Ok(article)
    }

    // 一時ファイルに書いてから置き換える。途中で終わっても既存のmdは壊れない
    fn write_md(&self, article: &Article) -> Result<(), StoreError> {
        let md = self.md_path(&article.slug);
        let dir = self.posts_dir(&article.slug);
        let mut tmp =
            tempfile::NamedTempFile::new_in(&dir).map_err(|error| StoreError::io(&dir, error))?;
        tmp.write_all(render_md(article).as_bytes())
            .map_err(|error| StoreError::io(tmp.path(), error))?;
        tmp.persist(&md)
            .map_err(|error| StoreError::io(&md, error))?;
        Ok(())
    }
}

// ファイル名1個分か。「..」は親フォルダへ抜けるので含めない
fn is_filename(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && !name.contains('\\') && !name.contains("..")
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
            let prefix = assets_ref(slug);
            let name = path
                .strip_prefix(&prefix)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| malformed(format!("heroImageが{prefix}の下を指していない")))?;
            Some(name.to_string())
        }
        None => None,
    };

    Ok(Article {
        slug: slug.to_string(),
        fields: ArticleFields {
            title: string("title", field("title")?)?,
            description: string("description", field("description")?)?,
            pub_date: field("pubDate")?.to_string(),
            hero_image,
            body: body.to_string(),
        },
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
    fn 存在しない記事は読めない() {
        let (_tmp, store) = setup();
        assert!(matches!(
            store.read_article("../outside"),
            Err(StoreError::InvalidSlug { .. })
        ));
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

        let list = store.list_articles();
        assert_eq!(slugs(&list), ["c-post", "a-post", "b-post", "d-post"]);
    }

    #[test]
    fn rootが無ければ一覧は空() {
        let (_tmp, store) = setup();
        assert!(store.list_articles().is_empty());
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
        // 直前の内容がバックアップに残った状態にしてから名前を変える
        let revised = ArticleFields {
            title: "改訂".to_string(),
            pub_date: "2026-04-02".to_string(),
            ..first
        };
        store.save_article("old-slug", revised).unwrap();

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
        // どの保存も反映されていない
        let article = store.read_article("checks").unwrap();
        assert_eq!(article.fields.title, "");
        assert_eq!(article.fields.pub_date, today());
    }
}
