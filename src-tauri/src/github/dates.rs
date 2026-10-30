//! 手で直した日付の印(docs/spec.mdの「日付の自動の更新」)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use super::commands::CommandError;
use super::config::RepoRef;
use super::state::{self, Basis, State};
use super::GithubError;
use crate::store::{DateField, EditedFields, SavedDates, Store};

// 印の書き込みを1つずつにする。保存はStoreのロックより先にこれを取る
pub static MARKS_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    MARKS_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

// 記事ごとの、手で直した日付
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DateMarks {
    pub pub_date: bool,
    pub updated_date: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Marks {
    pub articles: BTreeMap<String, DateMarks>,
}

pub fn marks_path(data_dir: &Path, repo: &RepoRef) -> PathBuf {
    state::repo_dir(data_dir, repo).join("dates.json")
}

// 無ければ空
pub fn read_marks(path: &Path) -> Result<Marks, GithubError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Marks::default()),
        Err(error) => return Err(GithubError::io(path, error)),
    };
    serde_json::from_slice(&bytes).map_err(|error| GithubError::io(path, error))
}

// どちらの印も無い記事は項目ごと消す
pub fn write_marks(path: &Path, marks: &Marks) -> Result<(), GithubError> {
    let mut marks = marks.clone();
    marks
        .articles
        .retain(|_, marks| marks.pub_date || marks.updated_date);
    state::write_replacing(
        path,
        &serde_json::to_vec(&marks).expect("Marks should serialize"),
    )
}

// ベースの様子が無ければNone。公開済みの記事はUpdatedDate、執筆中はPubDate
pub fn auto_field(state: &State, slug: &str) -> Option<DateField> {
    let base = state.remote.base.as_ref()?;
    if base.articles.contains_key(slug) || state::original_slug(state, slug).is_some() {
        Some(DateField::UpdatedDate)
    } else {
        Some(DateField::PubDate)
    }
}

// updated_dateは、updatedDateが有効か
pub struct SaveInput<'a> {
    pub data_dir: &'a Path,
    pub repo: &'a RepoRef,
    pub basis: &'a Basis,
    pub store: &'a Store,
    pub updated_date: bool,
    pub lock: &'a Mutex<()>,
}

pub fn save(
    input: &SaveInput<'_>,
    slug: &str,
    fields: EditedFields,
    today: &str,
) -> Result<SavedDates, CommandError> {
    let _guard = input.lock.lock().unwrap_or_else(PoisonError::into_inner);
    let path = marks_path(input.data_dir, input.repo);
    // 印を読めないときは、日付を自動で替えず、印も書かない
    let mut marks = match read_marks(&path) {
        Ok(marks) => marks,
        Err(error) => {
            eprintln!("Failed to read the date marks of {slug}: {error}");
            return Ok(input.store.save_article(slug, fields, None)?);
        }
    };

    // 送られた日付の印は、mdより先に書く
    let sent_marks = {
        let marks = marks.articles.entry(slug.to_string()).or_default();
        let mut changed = false;
        if fields.pub_date.is_some() && !marks.pub_date {
            marks.pub_date = true;
            changed = true;
        }
        if fields.updated_date.is_some() && !marks.updated_date {
            marks.updated_date = true;
            changed = true;
        }
        changed
    };
    if sent_marks {
        write_marks(&path, &marks)?;
    }

    // 分類が分からないときは、日付を自動で替えない
    let field = match state::read_state(&state::state_path(input.data_dir, input.repo), input.basis)
    {
        Ok(state) => auto_field(&state, slug),
        Err(error) => {
            eprintln!("Failed to read the GitHub state of {slug}: {error}");
            None
        }
    };
    let auto = field.and_then(|field| {
        let marks = marks.articles.get(slug);
        let marked = match field {
            DateField::PubDate => marks.is_some_and(|marks| marks.pub_date),
            DateField::UpdatedDate => {
                !input.updated_date || marks.is_some_and(|marks| marks.updated_date)
            }
        };
        (!marked).then_some((field, today))
    });
    let saved = input.store.save_article(slug, fields, auto)?;

    // 公開済みの記事のpubDateの印は、公開されたので外す
    if field == Some(DateField::UpdatedDate)
        && marks.articles.get(slug).is_some_and(|marks| marks.pub_date)
    {
        if let Some(entry) = marks.articles.get_mut(slug) {
            entry.pub_date = false;
        }
        if let Err(error) = write_marks(&path, &marks) {
            eprintln!("Failed to clear the pubDate mark of {slug}: {error}");
        }
    }
    Ok(saved)
}

pub fn clear_marks(path: &Path, slug: &str) -> Result<(), GithubError> {
    let _guard = lock();
    let mut marks = read_marks(path)?;
    if marks.articles.remove(slug).is_some() {
        write_marks(path, &marks)?;
    }
    Ok(())
}

pub fn clear_updated_mark(path: &Path, slug: &str) -> Result<(), GithubError> {
    clear_updated_mark_if(path, slug, || true)
}

// whenがtrueを返したときだけ外す。印のロックを持ったまま確かめるので、その間の保存で付いた印は外さない
pub fn clear_updated_mark_if(
    path: &Path,
    slug: &str,
    when: impl FnOnce() -> bool,
) -> Result<(), GithubError> {
    let _guard = lock();
    if !when() {
        return Ok(());
    }
    let mut marks = read_marks(path)?;
    let Some(entry) = marks.articles.get_mut(slug) else {
        return Ok(());
    };
    if !entry.updated_date {
        return Ok(());
    }
    entry.updated_date = false;
    write_marks(path, &marks)?;
    Ok(())
}

// newに残っていた印(読み込みで手元から消した記事の印など)は捨てる
pub fn rename_marks(path: &Path, old: &str, new: &str) -> Result<(), GithubError> {
    let _guard = lock();
    let mut marks = read_marks(path)?;
    let stale = marks.articles.remove(new).is_some();
    let moved = marks.articles.remove(old);
    let changed = stale || moved.is_some();
    if let Some(date_marks) = moved {
        marks.articles.insert(new.to_string(), date_marks);
    }
    if changed {
        write_marks(path, &marks)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use tempfile::TempDir;

    use super::*;
    use crate::github::config::github_posts_dir;
    use crate::github::fake::repo_ref;
    use crate::github::state::{self, ArticleFiles, BaseSnapshot, BranchSnapshot, Record};
    use crate::store::{DateField, EditedFields, SavedDates, Store};

    const POSTS_PATH: &str = "src/content/blog";

    fn basis() -> Basis {
        Basis {
            base_branch: "main".to_string(),
            branch_format: "blog/{slug}".to_string(),
            posts_path: POSTS_PATH.to_string(),
        }
    }

    fn files() -> ArticleFiles {
        ArticleFiles {
            md: "md".to_string(),
            assets: BTreeMap::new(),
        }
    }

    // ベースのブランチにslugの記事がある状態
    fn base_state(slugs: &[&str]) -> State {
        let mut state = State::empty(basis());
        state.remote.base = Some(BaseSnapshot {
            commit: "base".to_string(),
            articles: slugs
                .iter()
                .map(|slug| ((*slug).to_string(), files()))
                .collect(),
            shared_assets: None,
        });
        state
    }

    struct DatesTest {
        _tmp: TempDir,
        data_dir: PathBuf,
        store: Store,
        repo: RepoRef,
        basis: Basis,
        state_path: PathBuf,
        marks_file: PathBuf,
        lock: Mutex<()>,
    }

    impl DatesTest {
        fn new() -> Self {
            let tmp = TempDir::new().unwrap();
            let data_dir = tmp.path().join("data");
            let repo = repo_ref();
            let basis = basis();
            let posts_dir = github_posts_dir(&data_dir, &repo, POSTS_PATH);
            let store = Store::new(posts_dir, tmp.path().join("backup")).unwrap();
            let state_path = state::state_path(&data_dir, &repo);
            let marks_file = marks_path(&data_dir, &repo);
            Self {
                data_dir,
                store,
                repo,
                basis,
                state_path,
                marks_file,
                lock: Mutex::new(()),
                _tmp: tmp,
            }
        }

        fn input(&self) -> SaveInput<'_> {
            SaveInput {
                data_dir: &self.data_dir,
                repo: &self.repo,
                basis: &self.basis,
                store: &self.store,
                updated_date: true,
                lock: &self.lock,
            }
        }

        fn save(
            &self,
            slug: &str,
            fields: EditedFields,
            today: &str,
        ) -> Result<SavedDates, CommandError> {
            save(&self.input(), slug, fields, today)
        }

        // ベースのブランチの様子を書く
        fn write_state(&self, slugs: &[&str]) {
            state::write_state(&self.state_path, &base_state(slugs)).unwrap();
        }

        fn marks(&self) -> Marks {
            read_marks(&self.marks_file).unwrap()
        }

        fn write_marks_of(&self, entries: &[(&str, bool, bool)]) {
            let mut marks = Marks::default();
            for (slug, pub_date, updated_date) in entries {
                marks.articles.insert(
                    (*slug).to_string(),
                    DateMarks {
                        pub_date: *pub_date,
                        updated_date: *updated_date,
                    },
                );
            }
            write_marks(&self.marks_file, &marks).unwrap();
        }

        // 日付は送らない(手で直していない)保存の入力
        fn fields_of(&self, slug: &str) -> EditedFields {
            let article = self.store.read_article(slug).unwrap();
            EditedFields {
                title: article.fields.title,
                description: article.fields.description,
                pub_date: None,
                updated_date: None,
                body: article.fields.body,
            }
        }

        fn md(&self, slug: &str) -> String {
            fs::read_to_string(self.store.posts_dir().join(format!("{slug}.md"))).unwrap()
        }
    }

    #[test]
    fn auto_fieldはベースにある記事を公開済みとする() {
        // ベースの様子が無ければ分からない
        let state = State::empty(basis());
        assert_eq!(auto_field(&state, "any"), None);

        let mut state = base_state(&["published"]);
        // ベースにある記事は公開済み
        assert_eq!(
            auto_field(&state, "published"),
            Some(DateField::UpdatedDate)
        );
        // ベースに無い記事は執筆中
        assert_eq!(auto_field(&state, "draft"), Some(DateField::PubDate));

        // 公開済みの記事からslugを変えた記事
        state.articles.insert(
            "renamed".to_string(),
            Record {
                renamed_from: Some("published".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(auto_field(&state, "renamed"), Some(DateField::UpdatedDate));

        // 記録のブランチが移した記事
        state.remote.branches.insert(
            "blog/moved".to_string(),
            BranchSnapshot {
                commit: "moved".to_string(),
                slug: "moved".to_string(),
                article: None,
                moved_from: Some("published".to_string()),
            },
        );
        assert_eq!(auto_field(&state, "moved"), Some(DateField::UpdatedDate));
    }

    #[test]
    fn saveは執筆中ならpubDateを公開済みならupdatedDateを今日にする() {
        let test = DatesTest::new();
        test.write_state(&["published"]);

        // 執筆中の記事
        test.store.create_article("draft").unwrap();
        let mut fields = test.fields_of("draft");
        fields.body = "変えた本文".to_string();
        let saved = test.save("draft", fields, "2000-05-01").unwrap();
        assert_eq!(saved.pub_date.as_deref(), Some("2000-05-01"));
        assert_eq!(saved.updated_date, None);

        // 公開済みの記事
        let created = test.store.create_article("published").unwrap();
        let mut fields = test.fields_of("published");
        fields.body = "変えた本文".to_string();
        let saved = test.save("published", fields, "2000-05-01").unwrap();
        assert_eq!(saved.updated_date.as_deref(), Some("2000-05-01"));
        assert_eq!(saved.pub_date, created.fields.pub_date);
    }

    #[test]
    fn 手で直した日付には印が付き次の保存で自動で替わらない() {
        let test = DatesTest::new();
        test.write_state(&["published"]);
        test.store.create_article("draft").unwrap();
        test.store.create_article("published").unwrap();

        // 送ったpubDateには印が付き、その保存では送った値になる
        let mut fields = test.fields_of("draft");
        fields.pub_date = Some("2026-02-02".to_string());
        fields.body = "最初の本文".to_string();
        let saved = test.save("draft", fields, "2026-01-02").unwrap();
        assert_eq!(saved.pub_date.as_deref(), Some("2026-02-02"));
        assert!(test.marks().articles["draft"].pub_date);

        // 次に本文だけ変えてもpubDateは替わらない
        let mut fields = test.fields_of("draft");
        fields.body = "変えた本文".to_string();
        let saved = test.save("draft", fields, "2026-05-01").unwrap();
        assert_eq!(saved.pub_date.as_deref(), Some("2026-02-02"));

        // 送ったupdatedDateにも印が付き、次の保存で足されない
        let mut fields = test.fields_of("published");
        fields.updated_date = Some(Some("2026-02-02".to_string()));
        fields.body = "最初の本文".to_string();
        let saved = test.save("published", fields, "2026-01-02").unwrap();
        assert_eq!(saved.updated_date.as_deref(), Some("2026-02-02"));
        assert!(test.marks().articles["published"].updated_date);

        let mut fields = test.fields_of("published");
        fields.body = "変えた本文".to_string();
        let saved = test.save("published", fields, "2026-05-01").unwrap();
        assert_eq!(saved.updated_date.as_deref(), Some("2026-02-02"));

        // 外したときも印が付き、次の保存で足されない
        let mut fields = test.fields_of("published");
        fields.updated_date = Some(None);
        fields.body = "外した本文".to_string();
        test.save("published", fields, "2026-01-03").unwrap();
        assert!(test.marks().articles["published"].updated_date);
        let mut fields = test.fields_of("published");
        fields.body = "また変えた本文".to_string();
        let saved = test.save("published", fields, "2026-05-01").unwrap();
        assert_eq!(saved.updated_date, None);
    }

    #[test]
    fn 公開済みの記事の保存でpubDateの印は外れupdatedDateの印は残る() {
        let test = DatesTest::new();
        test.write_state(&["published"]);
        test.store.create_article("published").unwrap();
        test.write_marks_of(&[("published", true, true)]);

        let mut fields = test.fields_of("published");
        fields.body = "変えた本文".to_string();
        test.save("published", fields, "2026-05-01").unwrap();

        let marks = test.marks();
        assert!(!marks.articles["published"].pub_date);
        assert!(marks.articles["published"].updated_date);
    }

    #[test]
    fn updatedDateを無効にすると公開済みの記事のupdatedDateは替えない() {
        let test = DatesTest::new();
        test.write_state(&["published"]);
        test.store.create_article("published").unwrap();
        test.store.create_article("draft").unwrap();
        let disabled = SaveInput {
            updated_date: false,
            ..test.input()
        };

        let mut fields = test.fields_of("published");
        fields.body = "変えた本文".to_string();
        let saved = save(&disabled, "published", fields, "2026-05-01").unwrap();
        assert_eq!(saved.updated_date, None);

        // 執筆中の記事のpubDateは替わる
        let mut fields = test.fields_of("draft");
        fields.body = "変えた本文".to_string();
        let saved = save(&disabled, "draft", fields, "2026-05-01").unwrap();
        assert_eq!(saved.pub_date.as_deref(), Some("2026-05-01"));
    }

    #[test]
    fn dates_jsonが読めなければ日付も印も替えない() {
        let test = DatesTest::new();
        test.write_state(&["draft"]);
        test.store.create_article("draft").unwrap();
        fs::create_dir_all(test.marks_file.parent().unwrap()).unwrap();
        fs::write(&test.marks_file, "@").unwrap();

        let created = test.store.read_article("draft").unwrap();
        let mut fields = test.fields_of("draft");
        fields.body = "変えた本文".to_string();
        let saved = test.save("draft", fields, "2000-05-01").unwrap();
        assert_eq!(saved.pub_date, created.fields.pub_date);

        let mut fields = test.fields_of("draft");
        fields.pub_date = Some("2026-02-02".to_string());
        fields.body = "また変えた本文".to_string();
        let saved = test.save("draft", fields, "2000-05-01").unwrap();
        assert_eq!(saved.pub_date.as_deref(), Some("2026-02-02"));
        assert_eq!(fs::read_to_string(&test.marks_file).unwrap(), "@");
    }

    #[test]
    fn state_jsonが読めなければ日付を替えずに保存する() {
        let test = DatesTest::new();
        test.store.create_article("draft").unwrap();
        fs::create_dir_all(test.state_path.parent().unwrap()).unwrap();
        fs::write(&test.state_path, "@").unwrap();

        let created = test.store.read_article("draft").unwrap();
        let mut fields = test.fields_of("draft");
        fields.body = "変えた本文".to_string();
        let saved = test.save("draft", fields, "2000-05-01").unwrap();
        assert_eq!(saved.pub_date, created.fields.pub_date);
        assert_eq!(
            test.store.read_article("draft").unwrap().fields.body,
            "変えた本文"
        );
    }

    #[cfg(unix)]
    #[test]
    fn 印を書けなければ手で直した日付の保存は失敗する() {
        let test = DatesTest::new();
        test.write_state(&["draft"]);
        test.store.create_article("draft").unwrap();
        test.write_marks_of(&[("draft", false, false)]);
        let before = test.md("draft");
        let Some(read_only) = crate::github::fake::read_only_dir(test.marks_file.parent().unwrap())
        else {
            return;
        };

        let mut fields = test.fields_of("draft");
        fields.pub_date = Some("2026-02-02".to_string());
        fields.body = "変えた本文".to_string();
        let result = test.save("draft", fields, "2026-05-01");
        drop(read_only);
        assert!(result.is_err());
        assert_eq!(test.md("draft"), before);
    }

    #[test]
    fn 印はslugの変更で付け替わり削除で外れる() {
        let test = DatesTest::new();
        test.write_marks_of(&[("old", true, true), ("other", true, false)]);

        rename_marks(&test.marks_file, "old", "new").unwrap();
        let marks = test.marks();
        assert!(!marks.articles.contains_key("old"));
        assert!(marks.articles["new"].pub_date);
        assert!(marks.articles["new"].updated_date);
        assert!(marks.articles["other"].pub_date);

        clear_updated_mark(&test.marks_file, "other").unwrap();
        let marks = test.marks();
        assert!(!marks.articles["other"].updated_date);
        assert!(marks.articles["other"].pub_date);

        // 移す先に残っていた印は、元に印が無くても消える
        test.write_marks_of(&[
            ("new", true, true),
            ("other", true, false),
            ("stale", true, false),
        ]);
        rename_marks(&test.marks_file, "none", "stale").unwrap();
        assert!(!test.marks().articles.contains_key("stale"));

        clear_marks(&test.marks_file, "new").unwrap();
        let marks = test.marks();
        assert!(!marks.articles.contains_key("new"));
        assert!(marks.articles["other"].pub_date);
    }
}
