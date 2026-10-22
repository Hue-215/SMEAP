---
name: pr-review
description: PR作成前のレビュー手順および、結果の残し方。PRを作る前に開く。
---

# PR作成前のレビュー手順

PR作成前に、次の順に進める。後の段が前の段の直しもまとめて見るので、順番を入れ替えない。

1. `/simplify`で、変更したコードを整える。
2. 差分がセキュリティに関わる(外部通信・秘密情報・権限設定・外から受け取る入力・プロンプト注入対策・実行時依存の追加など)ときは、claude-securityの差分スキャンを回し、報告された指摘を直す。commitしていない変更は対象外なので、先にcommitする。`/claude-security`はClaudeからは呼べない(`disable-model-invocation`)ので、ユーザーに`/claude-security scan changes --base <ベースブランチ> --effort medium`の実行を頼む。
3. `pr-review-toolkit:review-pr`でレビューを行う。デフォルトだとcommitしていない範囲のみを参照するので、`<ベースブランチ>...HEAD`を指定して呼ぶ。観点は`code tests errors comments types`を指定する(`simplify`は1で済んでいる)。

レビューの扱い:

- コードの差分以上の文脈を渡さない。実装に関する文脈を与えると、それに引っ張られてレビュー役の視野が狭まる。
- 新規の振る舞いを求める指摘は、直す前に`gh issue list`で既存のIssueが扱っていないかを確かめる。既存のIssueの範囲なら、このPRでは直さない。

PRを作ったあと、PRのコメントに残す内容は実装した側が判断する。今後の作業で要りそうな情報と、PRの内容を作るときに使った判断材料(レビューの指摘とその扱いを含む)を書く。
