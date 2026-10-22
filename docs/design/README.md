# 画面試作のソース

仕様を決めるときに使った画面試作(claude.aiのArtifact、Design形式)のソース。2026-10-22時点の版を写したもの。

- 試作: https://claude.ai/artifact/8bn9qixundRorooQHvkTdF (持ち主だけが開ける)
- 決まった内容は[spec.md](../spec.md)にある。試作と食い違うときはspec.mdに従う

## ファイル

| ファイル | 画面 |
|---|---|
| `canvas.json` | 画面の並び(キャンバス上の位置と大きさ、名前) |
| `Main.dc.html` | デスクトップ 編集画面(サイドバーの開閉、記事の情報の折りたたみが動く) |
| `Settings.dc.html` | デスクトップ 設定画面 |
| `NewPost.dc.html` | 新しい記事のダイアログ(slugの入力) |
| `SaveConfirm.dc.html` | 保存の確認のダイアログ |
| `Android.dc.html` | Android 編集画面(その他のメニューが開く) |
| `AndroidDrawer.dc.html` | Android 記事一覧 |
| `Colors.dc.html` | アクセント色の見本 |

## 読み方

- 各`.dc.html`は1画面分。`<x-dc>`の中が画面のHTML、最後の`<script type="text/x-dc">`が値の計算
- 色は各ファイルの`renderVals()`にある`t`(ライト・ダークの2組)にまとまっている
- `{{名前}}`は`renderVals()`が返す値に置き換わる
- `<sc-if>`は条件つきの表示、`<sc-for>`は繰り返し
- `support.js`はArtifactの形式が用意するもので、ここには無い。このままブラウザで開いても表示されない

## 別のセッションで試作を直すとき

- 上の試作のURLをClaudeに渡して、Artifactを読み込んでから直す。直したら、このフォルダにも写す
- Artifactを開けない場合は、このフォルダのファイルをもとに、新しいDesign形式のArtifactを作り直す
