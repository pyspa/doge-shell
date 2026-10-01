# 対話ジョブと Tool Search

`!` チャット（経路 A）の managed job と Tool Search の設計。AI 機能の設計方針全体の索引は
[README.md](README.md) を見る。

経路 A の `execute` も `AgentJobs` を通るようになった（`dsh-builtin/src/chatgpt/tool/execute/jobs.rs`）。
その際に受け入れたトレードオフ:

- **出力は末尾 1MiB のみ**。旧 `CappedCapture` は先頭 512KiB + 末尾 512KiB を保っていたが、
  `AgentJobs` のリングは tail-only。`snapshot` の `base = total - bytes.len()` というオフセット
  算術が純リングバッファ前提なので、head+tail 化すると `*_next_offset` の意味が壊れる。
  `render_result` がどのみち 3072 字に中央切り詰めするので、実害は 1MiB 超の出力に限られる。
- **正常終了時にも `killpg` する**。`AgentJobs` の worker は `try_wait` が成功した直後にも
  プロセスグループを落とすので、`execute` から `foo &` で残したプロセスは殺される。回避は
  `setsid`。
- **連続ポーリング下限は対話だけ**（`chatgpt/jobs.rs` の `poll_backoff`）。`runtime.jobs` には
  `JobMeta` が無い。`wait_ms` は既定 0 なので opt-in。
- **`tool_search` は対話にも開いている**。`tool_search` is available to interactive turns.
  Interactive Tool Search loads only the matched MCP schemas into turn-local
  exposure. It does not enable the corresponding MCP group and does not persist
  the exposure beyond the current turn.
  `mcp_load_group` remains available when the model needs a whole group.
  対話ターンは全 MCP schema を常時載せない。載せるのは active group の schema と
  turn-local な Tool Search ヒットのみで、inactive group の schema は載せない。
  inactive な schema は `mcp_list_groups` / `mcp_load_group` の meta tool 経路で
  activation するか、`tool_search` で個別に発見し、`tool_search` による変更は
  次 iteration の再構築で反映される。  `tool_search` は group の enabled 状態を変えない。

## MCP モデル表面の二つの mode（Eager / Bridge）

`AI_CHAT_MCP_TOOL_MODE`（`auto` 既定）が `!` チャットの MCP 表面を決める。
mode は turn 開始時に1回だけ解決し（`TurnSetup::mcp_tool_mode`）、その turn の
全 iteration で不変。iteration ごとに変えると provider prefix cache が毎回
外れるため、再計算しない。

- **Eager**: 既存動作。active group の schema＋turn-local な Tool Search 公開
  （`ToolSearchExposure`）を毎リクエスト再構築する。`mcp_list_groups` /
  `mcp_load_group` も載る。小さな catalog では発見の1往復より schema 直載せが
  安いため、互換 fallback としても残る。
- **Bridge**: 安定した `tool_search` / `tool_describe` / `tool_call` のみ。
  `mcp__*` schema・`mcp_list_groups` / `mcp_load_group` は載せない。
  `tool_search` は compact な候補だけ返し、次のリクエストを変えない。
  full schema は `tool_describe` が都度解決し、実行は `tool_call` が論理 MCP
  呼び出しへ unwrap してから既存の policy（hooks / SafetyGuard / approval /
  AgentRuntime / MCP task）へ渡す。`tool_call` 自体は transport adapter で、
  第二の MCP 実行経路ではない。

`auto` は discoverable な MCP schema 合計（group 無効化は無視、disconnect・
stale は除外）が `12 KiB` を超えたら Bridge を選ぶ。group disable は
authorization ではなく exposure 制御なので、bridge の `tool_call` も通常通り
SafetyGuard を通る。

## Tool Search exposure budget

`tool_search` を 1 turn 内で繰り返すと schema が際限なく累積する問題への対処。
`dsh-builtin/src/chatgpt/tool/tool_search.rs` の `ToolSearchExposure` が
turn-local に以下を課す（定数。env var / config なし）。**この budget は
Eager mode のみに適用する。Bridge mode は schema を動的追加しない。**

- **tool数**: 1 turn に `tool_search` 経由で新規公開できるのは 32 tool まで
 （`MAX_TOOL_SEARCH_EXPOSED_TOOLS_PER_TURN`）。
- **schema bytes**: 同じく serialized schema（compact JSON の UTF-8 byte数、
  token数ではない）の合計 96 KiB まで
  （`MAX_TOOL_SEARCH_SCHEMA_BYTES_PER_TURN`）。

対象は `tool_search` による暗黙的・個別的な schema 追加だけ。builtin / job /
meta tool、active group から既に公開されている schema、同じ turn で既に
charge 済みの tool は消費しない。`mcp_load_group` による明示的な group 全体の
activation は別の control plane として対象外。budget は turn 中 monotonic
（disconnect 等で schema が消えても refund しない）で、次の user turn で
reset する。名前だけ保持し schema は `build_request_tools` で毎回再解決する
既存設計は維持する。

budget で skip が発生した `tool_search` の tool result には短い note を付けて
model へ伝える（全件 loaded の場合は付けない）。

## Tool Search eval

ranking 自体の回帰検出用。`dsh-builtin/src/chatgpt/tool/tests/` の
`tool_search_eval.rs` + `data/tool_search_eval.json`（45 tools / 66 queries、
完全 deterministic、外部接続なし）が生産 ranker を呼んで Hit@1/@3/@5、
Recall@1/@3/@5、MRR を測る。report は
`cargo test -p dsh-builtin tool_search_eval_report -- --ignored --nocapture`、
CI で走る floor は `tool_search_eval_regression`
（baseline Hit@5=1.000 / MRR=0.951 に対し floor 0.95 / 0.90）。
ranking（weight・BM25・fuzzy・embedding 等）を変えるときは eval 結果で正当化する。
