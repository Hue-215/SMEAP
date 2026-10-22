---
name: pr-review
description: PR作成前のレビュー手順と、結果の残し方。PRを作る前に開く。
---

# PR作成前のレビュー手順

pr-review-toolkitとclaude-securityのプラグインを使う。

PR作成前に、次の順に進める。後の段が前の段の直しもまとめて見るので、順番を入れ替えない。2と3はcommit済みの変更だけを見るので、各段で直した分はcommitしてから次へ進む。ベースブランチは`origin/develop`とし、始める前に`git fetch origin develop`で最新にする。

1. `/simplify`で、変更したコードを整える。
2. 差分がセキュリティに関わる(外部通信・秘密情報・権限設定・外から受け取る入力・プロンプト注入対策・実行時依存の追加など)ときは、claude-securityの差分スキャンを回し、報告された指摘を直す。`/claude-security`はClaudeからは呼べない(`disable-model-invocation`)ので、ユーザーに`/claude-security scan changes --base origin/develop --effort medium`の実行を頼む。
3. `pr-review-toolkit:review-pr`でレビューを行う。範囲を受け取る引数は無く、既定ではcommitしていない変更だけを見るので、観点の後ろに`origin/develop...HEAD`と書き添えて範囲を伝える。観点は`code tests errors comments types`を指定する(`simplify`は1で済んでいる)。

レビューの扱い:

- コードの差分以上の文脈を渡さない。実装に関する文脈を与えると、それに引っ張られてレビュー役の視野が狭まる。
- 新規の振る舞いを求める指摘は、直す前に、既存のIssueが扱っていないかを、閉じたものも含めて全件から確かめる。既存のIssueの範囲なら、このPRでは直さない。

PRを作ったあと、PRにコメントを付け、今後の作業で要りそうな情報と、PRの内容を作るときに使った判断材料(レビューの指摘とその扱いを含む)を書く。ほかに何を書くかは実装した側が判断する。
