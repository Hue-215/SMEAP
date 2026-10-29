# Astro公式blogテンプレートのフロントマター

withastro/astro の `examples/blog`(mainブランチ、astro ^7.4.2)を2026-10-22に確認した内容。

Astroそのものはフロントマターの項目を決めていない。項目はサイトごとに`src/content.config.ts`のスキーマで決める。以下はblogテンプレートでの決め方。

## 項目の一覧

| 項目 | 必須/任意 | 型(スキーマ) | 使われる場所 |
|---|---|---|---|
| title | 必須 | `z.string()` | 記事ページのH1と`<title>`・OGP、記事一覧の見出し、RSS |
| description | 必須 | `z.string()` | 記事ページの`<meta name="description">`・OGP、RSS |
| pubDate | 必須 | `z.coerce.date()` | 記事ページの日付、記事一覧の日付と並び順(新しい順)、RSS |
| updatedDate | 任意 | `z.coerce.date().optional()` | 記事ページで、あるときだけ日付の下に「Last updated on {日付}」を出す |
| heroImage | 任意 | `z.optional(image())` | 記事ページの上部(1020×510)、記事一覧のサムネイル(720×360) |

## 補足

- pubDate・updatedDateは`z.coerce.date()`なので、`'Jul 08 2022'`のような書き方も`2022-07-08`も通る。サンプル記事は前者
- heroImageは`image()`なので、記事ファイルからの相対パスで`src/`内の画像を指す。サンプルは`'../../assets/blog-placeholder-3.jpg'`。この仕様の出力の形なら`'../../assets/{slug}/01.png'`になる。この仕様ではサンプルの形も読む([spec.md](spec.md)の共有の画像)
- heroImageはOGP画像には使われない。記事ページはBaseHeadにtitleとdescriptionしか渡さないので、OGP画像は常に既定の画像になる
- 記事のURLはファイル名から決まる(`post.id`)。この仕様の`{slug}.md`ならslugがそのままURLになる
- サンプル記事5本は、どれもtitle・description・pubDate・heroImageの4つを書いている。updatedDateを書いた記事は無い
- テンプレートの記事置き場は`src/content/blog`。実際のブログで項目を変えている場合は、そちらの`content.config.ts`に合わせる

## 参照したファイル

- `src/content.config.ts`
- `src/layouts/BlogPost.astro`
- `src/components/BaseHead.astro`
- `src/pages/blog/index.astro`
- `src/pages/blog/[...slug].astro`
- `src/pages/rss.xml.js`
- `src/content/blog/*.md(x)`
