# 6. 環境変数の正典

[README.md](README.md) の §6。AI 機能・Herdr 連携の環境変数はこのファイルが正典。
デバッグ/テスト用の環境変数は `docs/ai/skills/doge-shell-repo/references/env-vars.md` にある。

解決順は **shell 変数のみ**。起動時 process environment は
`Environment::new()` で import 済みなので、runtime の `chatgpt::load_openai_config`
は `ShellProxy::get_var`（= shell `Environment`）だけを見る。`unset` は final で、
起動時 process の同名値は復活しない。新しいキーもこの形で読む。
`std::env::var` だけを見ない。

| キー | 既定 | 定義 |
|---|---|---|
| `AI_CHAT_API_KEY` → `OPENAI_API_KEY` → `OPEN_AI_API_KEY` | なし | `dsh-openai/src/config.rs` |
| `AI_CHAT_BASE_URL` → `OPENAI_BASE_URL` | `https://api.openai.com/v1/` | 同上 |
| `AI_CHAT_MODEL` → `OPENAI_MODEL` | `DEFAULT_MODEL` | 同上。`chat_model` / `(vset "AI_CHAT_MODEL" ...)` によるセッション中の変更は `Environment::reload_chat_model`（`dsh/src/environment/variables.rs`）が `integration_state.chat_model` slot へ即座に反映し、`!` チャットだけでなくコマンドパレットの AI アクション・ゴーストテキスト・`ai-watch`・`blocks explain\|fix`・`output-gen` にも再起動なしで効く |
| `AI_CHAT_TIMEOUT_SECS` | 180（5〜1800 に clamp） | 同上 |
| `AI_CHAT_REASONING_EFFORT` | なし（未設定時は `OPENAI_REASONING_MODEL_PREFIXES` 該当モデル＋`tools` 付きリクエストだけ `"none"`、それ以外はプロバイダ既定） | 同上。値は allow-list しない（`none`/`minimal`/`low`/`medium`/`high` などプロバイダ依存）。設定値が `tools` 付きリクエストで拒否された後は、このクライアントの `tools` 付きリクエストで `"none"` に強制される |
| `AI_CHAT_ALLOW_INSECURE_HTTP` | off | 同上 |
| `AI_SUMMARY_MODEL` | チャットモデル | `dsh-builtin/src/chatgpt/settings.rs` |
| `AI_CHAT_SESSION_TTL_SECS` | 1800（`0` で無効） | `dsh-builtin/src/chatgpt/session/mod.rs`。idle timeout で、時計は成功したターンだけが進める（巻き戻したターンは進めない） |
| `AI_CHAT_CONTEXT_TOKEN_BUDGET` | 100000 | `dsh-builtin/src/chatgpt/settings.rs` |
| `AI_CHAT_TURN_TOKEN_BUDGET` | 無制限 | 同上 |
| `AI_CHAT_STREAM` | on（`0`/`false`/`off`/`no` で無効） | 同上（`resolve_stream_enabled`） |
| `AI_CHAT_EXECUTE_ALLOWLIST` | なし | `dsh-builtin/src/chatgpt/tool/execute.rs` |
| `AI_CHAT_EXECUTE_YIELD_MS` | 10000（0〜60000 に clamp） | `dsh-builtin/src/chatgpt/settings.rs`（`resolve_execute_yield_ms`）。対話 `execute` がジョブハンドルを返すまでの待ち時間。ツール引数 `yield_time_ms` が優先 |
| `AI_CHAT_EXECUTE_TIMEOUT_MS` | 600000（1000〜3600000 に clamp） | 同上（`resolve_execute_timeout_ms`）。`!` チャットの `execute` の既定 timeout |
| `AI_MESSAGE_LANG` | なし | `dsh-builtin/src/chatgpt/settings.rs`（`response_language`） |
| `CHAT_PROMPT` | なし | 同上 |
| `SAFETY_LEVEL` | `normal` | `dsh-types/src/safety_policy.rs` |
| `DOGESH_EXECUTE_TOOL_CONFIG` | XDG の `openai-execute-tool.json` | `execute.rs` |
| `AI_CHAT_PROJECT_SKILLS` | on（`0`/`false`/`off`/`no` で off） | `dsh-builtin/src/chatgpt/settings.rs` |
| `AI_CHAT_SKILL_STAGING` | `task`（`always`/`off` も可） | `dsh-builtin/src/chatgpt/settings.rs`（`resolve_skill_staging`）。`always` は常時ステージ。`off` と既定の `task` は通常の書き込み確認へ進む（`task` がステージするのは本番では到達しないタスク経路だけ。README §2） |
| `AI_CHAT_SKILL_REFLECT` | off | `dsh-builtin/src/chatgpt/reflect.rs`。ターン末の tools 無し単発リクエストで skill 提案を試みる |
| `AI_CHAT_SKILL_REFLECT_MIN_TOOLS` | 5 | 同上。このツール呼び出し数未満のターンでは送らない |
| `AI_CHAT_SKILL_REFLECT_MODEL` | `AI_SUMMARY_MODEL` → チャットモデル | 同上 |
| `AI_CHAT_SKILL_AUTO_ARCHIVE_DAYS` | off（0 または未設定） | `dsh-builtin/src/chatgpt/skills/usage.rs`（`sweep`）。`created_by == "agent"` かつ unpinned かつ user scope の skill だけを、指定日数未読で archive する |
| `AI_CHAT_VERIFY_AFTER_MUTATION` | off（`1`/`true`/`on`/`yes` で on） | `dsh-builtin/src/chatgpt/settings.rs`（`resolve_verify_after_mutation`）。`!` チャットで変異ツール（`edit`/`str_replace`/`execute`/`skill_manage`/`mcp__*`）実行後の最初の最終回答を1回だけ差し戻し、何を確認したかの記述を求める。2回目の回答は必ず採用（最大1往復の追加のみ） |
| `AI_CHAT_HOOKS` | on（同上で off） | `dsh-builtin/src/chatgpt/hooks/config.rs` |
| `AI_CHAT_HOOK_TURN_BUDGET_MS` | 無制限（`0` も無制限） | 同上 |
| `DOGESH_AI_HOOKS_CONFIG` | XDG の `ai-hooks.json` | 同上 |
| `DOGESH_HOOK_DEPTH` | なし（hook プロセスにだけ立つ） | 同上。**プロセス環境だけを見る**（シェル変数で消せると無限再帰する）。同種の例外は下の Herdr 連携の `HERDR_*` / `DOGESH_HERDR_OWNER_PID` だけ |

`AI_MESSAGE_LANG` は**散文にだけ**効く。JSON を返させるリクエスト
（`AiRequestOptions::json_object`）に `apply_language` を付けない。フィールド名と
列挙値まで訳され、`risk_level` が `"危険"` で返ってくると比較対象のどれにも一致しない。
その JSON に人が読むフィールドが 1 つあるとき（`safe-run` の `explanation`）だけ、
`apply_language_to_field` でその**フィールド名を指定して**言語を要求する。

モデル名の既定値は `dsh_openai::DEFAULT_MODEL` ただ 1 つ。
`doctor` を含め、どこにも文字列で書き写さない。

所在を間違えやすいもの:

| 名前 | ある場所 |
|---|---|
| `apply_language` / `apply_language_to_field` / `json_object_format` / `strip_code_fence` | `dsh-openai/src/response.rs` |
| `interpret_response` / `answer_text` / `truncate_middle` / `limits` / `handle_stall` | `dsh-openai/src/turn.rs` |
| `ChatRequestOptions`（プロバイダへ送るフィールド） | `dsh-openai/src/client.rs` |
| `AiRequestOptions`（シェル側の意図） | `dsh/src/ai_features/service.rs`。**`dsh-openai` には無い** |
| `AgentPolicyHandles`（レベル・ガード・2 つの allowlist） | 同上 |

`dsh-openai` の公開 API は `send_chat` / `send_chat_streaming` + `ChatRequestOptions` だけ。

`send_chat_streaming` は SSE chunk を `dsh_openai::stream::DeltaAggregator` で集約し、
`send_chat` と同じ形の `Value` を返す。呼び出し元（経路 A のみ）は
`turn::interpret_response` 以降を一切変えていない。互換サーバへのフォールバックは
`stream` / `stream_options` を `DROPPABLE_FIELDS` に含めるだけで、新しい仕組みを増やしていない。

シェル側の `AiRequestOptions` は tools を既定で送らない。MCP が必要なリクエストだけ
`with_tools()` で opt-in し、未知の MCP binding は成功ではなく tool error として返す。
`AI_MESSAGE_LANG` または `AI_CHAT_MODEL`/`OPENAI_MODEL` の shell 変数が変わったときは
read-only answer cache（`dsh/src/ai_features/cache.rs`）を全消しする。cache のキーは
リクエスト内容だけで、言語もモデルも含めない（変更時の全消しで足りている）。
API キー名の優先順と未設定時の案内は `dsh-openai/src/config.rs` が正典。

## Herdr 連携の変数

| 変数 | 定義位置 | 用途 |
|---|---|---|
| `HERDR_ENV` / `HERDR_PANE_ID` / `HERDR_BIN_PATH` | `dsh/src/agent_lifecycle/herdr.rs`（`HerdrEnv::detect`） | Herdr pane 内で起動されたことの検出。**プロセス環境だけを見る**（`DOGESH_HOOK_DEPTH` と同じ理由。シェル変数で「Herdr 配下だ」と偽装・抑止できてはいけない） |
| `DOGESH_HERDR_OWNER_PID` | 同上 | 同一 pane 内の入れ子 `dogesh` が lifecycle authority を取り合わないためのガード。プロセス環境だけを見る |
| `DOGESH_HERDR_ENABLED` | `dsh/src/agent_lifecycle/agent_command.rs`（`herdr_enabled`） | Herdr 連携全体の ON/OFF。`1`/`true`/`on`/`yes` で有効、既定 OFF（Herdr pane 内でも自動では有効にならない） |
| `DOGESH_HERDR_AGENT_COMMANDS` | `dsh/src/agent_lifecycle/agent_command.rs` | `codex`/`claude` など前景で認識するエージェント CLI 名の `:` 区切りリスト。素の名前は追加、`-name` は既定リストから除外 |
| `DOGESH_HERDR_AGENT_HANDOFF` | 同上 | `0`/`false`/`off`/`no` で前景エージェントへの pane 明け渡し機能自体を無効化。既定 on |

`DOGESH_HERDR_ENABLED`/`DOGESH_HERDR_AGENT_COMMANDS`/`DOGESH_HERDR_AGENT_HANDOFF` は他の AI 機能の変数と同じく shell 変数として解決する（`Environment::get_var`。起動時 process environment は `Environment::new()` で import 済み）。`HERDR_*`/`DOGESH_HERDR_OWNER_PID` は `DOGESH_HOOK_DEPTH` と同じ理由でプロセス環境のみを見る。
