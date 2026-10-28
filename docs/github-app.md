# GitHub Appの登録

GitHub連携([spec.md](spec.md)の「GitHub連携」)で使うGitHub Appの設定。SMEAPはデバイスフローでログインするので、client secretと秘密鍵は使わない。

## 登録

GitHubの Settings → Developer settings → GitHub Apps → New GitHub App で、次のとおりに作る。ここに書いていない項目は既定のままにする。

| 項目 | 値 |
|---|---|
| GitHub App name | 空いている名前(URLの`/apps/{slug}`になる) |
| Homepage URL | このリポジトリのURL |
| Callback URL | 空 |
| Expire user authorization tokens | 有効 |
| Request user authorization (OAuth) during installation | 無効 |
| Enable Device Flow | 有効 |
| Webhook の Active | 無効 |
| Repository permissions の Contents | Read and write |
| Repository permissions の Metadata | Read-only(Contentsを選ぶと自動で付く) |
| Where can this GitHub App be installed? | 自分だけで使うなら Only on this account、配るなら Any account |

ほかの権限は付けない。PRやCIは扱わないので、Pull requestsやChecksの権限は要らない。

## コードに入れる値

作った後のAppの画面にある2つを、`src-tauri/src/github/mod.rs`の定数に入れる。どちらも公開してよい値。

- Client ID(`Iv23`で始まる) → `CLIENT_ID`
- Appの公開ページのURLの`/apps/`の後ろ → `APP_SLUG`(Repositoryの選択の「repositoryを追加」で`https://github.com/apps/{APP_SLUG}/installations/new`を開く)

Generate a new client secret と Generate a private key は押さない。

## 使う人の手順

1. Appのインストールのページ(SMEAPの「repositoryを追加」から開ける)で、ブログのrepositoryを選んで入れる
2. SMEAPの設定画面で「GitHubにログイン」を押し、出たコードをブラウザで入力する
3. Repositoryの一覧から、ブログのrepositoryを選ぶ
