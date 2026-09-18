# 対話ジョブ（実装済み・残る制約）

[../open-questions.md](../open-questions.md) から分割。AI 機能の設計方針全体の索引は
[../README.md](../README.md) を見る。

経路 A の `execute` も `AgentJobs` を通るようになった（`dsh-builtin/src/chatgpt/tool/execute/jobs.rs`）。
その際に受け入れたトレードオフ:

- **出力は末尾 1MiB のみ**。旧 `CappedCapture` は先頭 512KiB + 末尾 512KiB を保っていたが、
  `AgentJobs` のリングは tail-only。`snapshot` の `base = total - bytes.len()` というオフセット
  算術が純リングバッファ前提なので、head+tail 化すると `*_next_offset` の意味が壊れる。
  `render_result` がどのみち 3072 字に中央切り詰めするので、実害は 1MiB 超の出力に限られる。
- **正常終了時にも `killpg` する**。`AgentJobs` の worker は `try_wait` が成功した直後にも
  プロセスグループを落とすので、`execute` から `foo &` で残したプロセスは殺される。回避は
  `setsid`。agent 経路は元からこの挙動で、両経路が揃う方向の変更として受け入れた。
- **連続ポーリング下限は対話だけ**（`chatgpt/jobs.rs` の `poll_backoff`）。`runtime.jobs` には
  `JobMeta` が無く、agent の挙動を変えない方針を優先した。`wait_ms` は両経路に入っているが
  既定 0 なので opt-in。
- **`tool_search` は対話にも開いている**。`tool_search` is available to interactive turns.
  Interactive Tool Search loads only the matched MCP schemas into turn-local
  exposure. It does not enable the corresponding MCP group and does not persist
  the exposure beyond the current turn.
  `mcp_load_group` remains available when the model needs a whole group.
  対話ターンは全 MCP schema を常時載せない。載せるのは active group の schema と
  turn-local な Tool Search ヒットのみで、inactive group の schema は載せない。
  inactive な schema は `mcp_list_groups` / `mcp_load_group` の meta tool 経路で
  activation するか、`tool_search` で個別に発見し、`tool_search` による変更は
  次 iteration の再構築で反映される。`tool_search` は group の enabled 状態を変えない。
