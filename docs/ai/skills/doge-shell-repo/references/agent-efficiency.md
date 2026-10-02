# エージェント開発の改善と評価

## ケース別の判断

| ケース | 読む範囲・作業 | 正確性を保つ条件 |
|---|---|---|
| typo・指定済みの単純修正 | 対象箇所のみ。routerは省略可 | diffと関連リンクを確認 |
| 補完JSON追加 | completion-spec Skillと対象JSON | loaderの検証、新規ファイルの埋め込み条件はSkillに従う |
| 挙動不具合 | 最小再現→spec→入口と状態の所有者→回帰テスト | 原因仮説を区別できる失敗を先に確認 |
| execution・PTY | 専用Skillと該当invariant | ownership、contract、端末挙動を個別に検証 |
| 製品AI機能 | `docs/design/ai/README.md` から該当設計へ | 既存の設定・承認・runtime authorityを使用 |
| 複数crateのrefactor | 呼び出し元と共有型の利用者を検索 | 挙動変更と移動を分け、影響するpackageを検証 |
| OS依存・外部コマンド | platform-supportと既存harness | ホストの成功と他OSの成功を区別 |
| 長時間・再開作業 | `docs/ai/long-task-workflow.md` | 完了条件、現在地、判断、未解決事項を保持 |
| ガイド・Skill修正 | 失敗した依頼とroutingケースから着手 | checker成功と実エージェントの成功を区別 |

検証コマンドの正典は [test-scope.md](test-scope.md)。この表にコマンド一覧を複製しない。

## 探索と出力を絞る

- routerが選んだSkillから始める。`repo-general` なら [task-map.md](task-map.md) をキーワード検索し、該当節だけ読む。パス判明後は `--path` を追加できる。
- `rg -l` で候補ファイル、`rg -n` でシンボルと呼び出し元を特定し、必要な範囲だけ読む。全文検索は狭い検索で見つからないときに広げる。
- 大きなテストログは一時ファイルに保存し、終了コードと失敗部分を読む。`head` や `tee` の成功をテスト成功と取り違えない。
- 同じファイルや成功済みログを再取得する代わりに、パス・シンボル・結論を保持する。変更やコンテキスト喪失後は必要な箇所を読み直す。
- 権限、依存取得、端末制約による失敗は実装不具合と区別する。テストを弱めて環境エラーを隠さない。

## Skillを追加する条件

既存Skillに収まる修正は、そのSkillかreferenceへ戻す。独立したトリガー、固有の判断、繰り返し使う手順が揃う領域だけ新しいdomain Skillにする。routerに入口と実際の依頼文の回帰ケースを追加し、coreの常設公開数は増やさない。1回限りの作業記録や長いログはSkillへ入れない。

## 改善効果を測る

1. `scripts/check-agent-context-budget.py` で常時コンテキストのbytes・description数を確認する。入口Skillや必須referenceのbytesも比較する。bytesはトークン数ではない。
2. `scripts/eval-agent-routing.py` で日本語・英語・path・曖昧な依頼を確認する。新しい正例に加え、製品AIの依頼を開発ガイドへ誤誘導しない負例を含める。
3. 実エージェントの比較は [評価手順](../../../evals/README.md) に従い、同じモデル・設定・ケース・検証条件でbefore/afterを測る。補完、execution、移植性を含め、成功率・実行テスト・変更範囲を先に確認し、その後にtokens・所要時間・tool回数を見る。
4. token usageが取得できない実行は欠測のまま扱う。静的文書量やrouting成功率だけから実装精度・消費トークンの改善率を断定しない。実エージェント評価未実施なら、その旨を記録する。
