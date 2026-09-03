# AI 機能の設計方針

doge-shell が**製品として持つ** AI 機能の方針。`docs/ai/` の他の文書は「この repo を AI に
編集させるときの運用ルール」で、別物。ここは実装の話をする。

新しい AI 機能を足すとき、既存の AI 機能を直すときに最初に読む。

## 1. プロバイダは OpenAI 互換 chat/completions のみ

- エンドポイントは `{base_url}/chat/completions` 固定（`dsh-openai/src/config.rs`）。
- 認証は `Authorization: Bearer` 固定。
- したがって **Anthropic Messages API（`x-api-key` + `anthropic-version`）はそのままでは使えない**。
  OpenAI 互換ゲートウェイ経由で使う。Responses API も対象外。
- ローカル / 互換サーバ対応は 2 つの仕組みで済ませる。増やさない。
  - `DROPPABLE_FIELDS`（`client.rs`）: 400 で拒否された optional フィールドを 1 回落として再送し、
    以後そのクライアントでは送らない。
  - `AI_CHAT_ALLOW_INSECURE_HTTP`: `http://` の base URL を許可する。既定では https へ差し替え、
    stderr に 1 回警告する。

## 2. エージェントループは 2 つだけ

| | A: `!` チャット | B: シェル側 AI |
|---|---|---|
| 入口 | `dsh/src/shell/eval.rs` → `dsh-builtin/src/chatgpt.rs` | `dsh/src/ai_features/service.rs` |
| 実行 | 同期 | 非同期 |
| ツール | builtin 8 種 + MCP | MCP のみ |
| 反復上限 | `MAX_TOOL_ITERATIONS` (100) | `MAX_ASSIST_ITERATIONS` (10) |

3 つ目を作らない。単発リクエスト（`ai-commit` / `safe-run` / ゴーストテキスト）は
ループを持たず、`turn::answer_text` で応答を読む。

両者が守る方針は `dsh-openai/src/turn.rs` に置く。ここに無い方針を片方だけに書かない。

## 3. 再実装してはいけないもの

| やりたいこと | 正の置き場所 |
|---|---|
| API 設定の解決（キー / モデル / base URL / timeout） | `OpenAiConfig::from_getter`（builtin からは `chatgpt::load_openai_config`） |
| 応答の解釈（tool_calls / answer / `finish_reason` / stall） | `dsh_openai::turn::{interpret_response, answer_text}` |
| 長い出力の切り詰め | `dsh_openai::turn::truncate_middle` |
| 応答言語の指示 | `dsh_openai::apply_language` |
| config ディレクトリ / skills ディレクトリ | `dsh_builtin::config_paths`（`dsh` crate 内は `environment::get_config_file`） |
| コマンドの危険度判定 | `dsh/src/safety` の `SafetyGuard` と `dsh_types::safety_policy` |
| MCP | `Environment.integration_state.mcp_manager` ただ 1 つ |
| MCP の function name → 実ツール名 | `McpManager::tool_name_for` |
| JSON リクエストの言語指示 | `dsh_openai::apply_language_to_field`（散文フィールド 1 つに限定） |

`choices[0].message.content` を自分で読まない。`turn` が配列形式の content と
`finish_reason=length` / `content_filter` を扱う。直読みしていた 3 経路
（`commit_ai` / `safe_run` / `suggestion`）は、切られた応答を正常な答えとして扱っていた。

`dirs::config_dir()` を直接呼ばない。macOS では `~/Library/Application Support` を指すので、
XDG を使う installer や `config.lisp` のローダと食い違う。
`scripts/check-portability.py` が機械的に禁止している。

## 4. 安全ゲートは `SafetyGuard` ただ 1 つ

- level の単一ソースは `policy_state.safety_level`。`ShellProxy::safety_level()` はそこを読む。
  `SAFETY_LEVEL` 変数は**表示用のコピー**で、起動時に継承値から seed され、
  `(safety-level ...)` が両方を更新する。変数の側を真実として読まない。
- コマンド（`execute` ツール）は `AgentCommandPolicy::evaluate_agent_command`。
  パイプライン全体を判定し、ラッパー（`sudo` / `env` / `xargs` …）は透過する。
- MCP ツールは `AgentCommandPolicy::evaluate_agent_tool` → `SafetyGuard::check_mcp_tool`。
  Loose は素通り、Normal は read-only を素通り、Strict は必ず確認。
  経路 A と B で同じ判定を使う。片方だけ無条件 confirm にしない。
- **判定した行と実行する行を一致させる**。`sh -c` は行全体を実行するので、
  guard がその一部しか読めないなら approve ではなく refuse する。
  - コマンド置換（`` ` ``, `$(...)`, `<(...)`, `(...)`）— `shell::parse::parse_command` が
    評価してしまうので、「安全か」を尋ねること自体が実行になる。
  - 文法が最後まで消費できない行（heredoc など）— `get_jobs` は警告するだけ。
  - 複合文（`{ ... }`, `for`, `if`, `while` …）— 中のコマンドは分類できない。
  - 文字列をコードとして渡す経路 — `-c` 系フラグに加えて、stdin から読むシェル
    （`printf ... | sh`）、入力リダイレクト（`bash < script.sh`）、`eval`。
- 分類は**行全体を対象に**。`Job.cmd` はパイプラインもまとめて 1 本の文字列なので、
  オペレータで区切り、ラッパー（`sudo` / `timeout` / `env` …）を覗いてから
  各段を分類する（`split_command_segments` + `command_candidates`）。
  先頭トークンだけを見ると `true | rm -rf ~` も `sudo rm -rf ~` も素通りする。
- MCP ツールの危険度は **function name ではなく実ツール名**で判定する。モデルが呼ぶ名前は
  `mcp__<label>__<tool>` なので、`"bash"` との完全一致は**一度も成立しない**。
  `check_mcp_tool(function_name, tool_name, ...)` の第 2 引数がそれで、
  `McpManager::tool_name_for` が引く。allowlist entry とユーザーへの質問文は
  function name のまま（ユーザーが見て承認したのはそちら）。read-only 判定も同じ理由で
  実ツール名を見る。ラベルはサーバの通称なので、`runner` という名前だけで
  その全ツールを mutating 扱いにしない。
- `SafetyResult` は **`Allowed | Confirm` の 2 値**。ガードは人が答えられる場所で走るので、
  一番強い返答は質問。拒否はエージェント経路の `AgentCommandVerdict::Denied` が担う。
  以前は `Denied` が 1 箇所からも生成されず、到達不能なハンドラが 9 箇所あった。
- **常に `None` を返す checker を登録しない**。登録されていることと効いていることの区別が
  つかなくなり、README が起きない確認を約束していた（`check_mv` / `check_package_manager`）。
- allowlist は 3 種類あり、意味が違う。混ぜない。
  - 設定 allowlist（`(chat-execute-add ...)` / JSON / env、`policy_state.execute_allowlist`）
    — **トークン前方一致**。人が書いた。エージェント経路は**読むだけ**。
  - エージェントのセッション承認（`policy_state.agent_session_allowlist`）— **完全一致**。
    `rm -rf target` の承認が `rm -rf target ~/documents` に及んではいけない。
    経路 A（`AgentCommandPolicy::remember_agent_approval`）と経路 B（`LiveAiService`）は
    **同じ箱**に書く。以前 B だけが設定 allowlist に書いていた。
  - ユーザー自身の "always"（`shell_always_allowlist`）— **AI には渡らない**。
    自分に許可したことは AI に許可したことではない。
- セッション承認のキーは接頭辞で区別する。いずれも完全一致。
  | キー | 意味 |
  |---|---|
  | 素のコマンド行 | `execute` がその行を再確認しない |
  | `mcp:<function_name>:<args>` | その MCP 呼び出しを再確認しない |
  | `write:<canonical path>` | `edit` / `str_replace` がそのファイルを再確認しない |
  | `sensitive:<action>:<canonical path>` | 機微パスの read / list / search を再確認しない |
- 承認 UI は `ApprovalDecision`（Allow / AllowAlways / Deny）ただ 1 つ。
  `dsh-builtin/src/chatgpt/tool/mod.rs` の `confirm_agent_action` を通す。
  **質問文に "Proceed?" を書かない**。`repl/confirmation.rs` が
  `Proceed? [y/N/a(Always)]:` を付けるので、書くと 2〜3 回出る。
- `loose` は「コマンド・MCP・機微読み取りを素通りさせる」であって「全部素通り」ではない。
  **ファイル書き込み（`edit` / `str_replace`）と skill script はレベルに関係なく必ず確認する。**

## 5. AI 機能は既定 OFF

`dsh/src/suggestion.rs` の既定は `ai_backfill: false` / `auto_fix: false` /
`ai_explanation: false`。API キーがあるだけでは何も自動で走らない。
有効化は `config.lisp` の `(set-suggestion-ai-enabled t)` /
`(set-auto-fix-enabled t)` / `(pref-ai-explanation t)`。

`!` チャット、`Alt+d`、`aic`、`safe-run`、`ai-watch` は明示的な操作なので、この既定とは無関係。

## 6. 環境変数の正典

解決順は **シェル変数 → プロセス環境**。`chatgpt::load_openai_config` がその形。
新しいキーもこの順で読む。`std::env::var` だけを見ない。

| キー | 既定 | 定義 |
|---|---|---|
| `AI_CHAT_API_KEY` → `OPENAI_API_KEY` → `OPEN_AI_API_KEY` | なし | `dsh-openai/src/config.rs` |
| `AI_CHAT_BASE_URL` → `OPENAI_BASE_URL` | `https://api.openai.com/v1/` | 同上 |
| `AI_CHAT_MODEL` → `OPENAI_MODEL` | `DEFAULT_MODEL` | 同上 |
| `AI_CHAT_TIMEOUT_SECS` | 180（5〜1800 に clamp） | 同上 |
| `AI_CHAT_ALLOW_INSECURE_HTTP` | off | 同上 |
| `AI_SUMMARY_MODEL` | チャットモデル | `dsh-builtin/src/chatgpt.rs` |
| `AI_CHAT_SESSION_TTL_SECS` | 1800（`0` で無効） | `dsh-builtin/src/chatgpt/session.rs` |
| `AI_CHAT_CONTEXT_TOKEN_BUDGET` | 100000 | `dsh-builtin/src/chatgpt.rs` |
| `AI_CHAT_TURN_TOKEN_BUDGET` | 無制限 | 同上 |
| `AI_CHAT_STREAM` | on（`0`/`false`/`off`/`no` で無効） | 同上（`resolve_stream_enabled`） |
| `AI_CHAT_EXECUTE_ALLOWLIST` | なし | `dsh-builtin/src/chatgpt/tool/execute.rs` |
| `AI_MESSAGE_LANG` | なし | `dsh-builtin/src/chatgpt.rs`（`response_language`） |
| `CHAT_PROMPT` | なし | 同上 |
| `SAFETY_LEVEL` | `normal` | `dsh-types/src/safety_policy.rs` |
| `DSH_EXECUTE_TOOL_CONFIG` | XDG の `openai-execute-tool.json` | `execute.rs` |

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
`send_message` / `send_message_with_model` / 位置引数版 `send_chat_request` は削除した
（呼び出し元が無く、repo 最後の `choices[0].message.content` 直読みを抱えていた）。

`send_chat_streaming` は SSE chunk を `dsh_openai::stream::DeltaAggregator` で集約し、
`send_chat` と同じ形の `Value` を返す。呼び出し元（経路 A のみ）は
`turn::interpret_response` 以降を一切変えていない。互換サーバへのフォールバックは
`stream` / `stream_options` を `DROPPABLE_FIELDS` に含めるだけで、新しい仕組みを増やしていない。

## 7. 未解決の設計判断

いずれも調査済みで根拠がある。着手する前にここを更新すること。

### 経路 A / B の非対称（調査済み・未着手）

- **経路 B は cancel callback を渡さない**（`dsh/src/ai_features/service.rs`、
  `ChatClient for ChatGptClient` が `send_chat(.., None)`）。`!` チャットは
  `proxy.is_canceled()` を渡すので、Ctrl-C はチャットにだけ効く。
- **経路 B は未登録のツール名を成功として返す**。`McpManager::execute_tool` の
  `Ok(None)`（binding 無し）を `"Tool executed successfully (no output)"` に落とすので、
  ハルシネーションした呼び出しが成功と伝わる。経路 A は
  `has_tool_binding` を先に見て `unsupported tool` を返す。
- **`AiRequestOptions::allow_tools` の既定が `true`**。ほとんどの呼び出しは
  `without_tools()` を書いているが、`diagnose_output` / `diagnose_output_with_history` /
  `send_followup_question` と `ShellProxy::ask_ai_async` は既定のままなので、
  MCP スキーマ全部が毎回添付される。`prompt_cache_key` も同 3 経路だけ未設定。
- **`ask_ai_async` は temperature 0.7 固定**。`blocks fix`（「修正コマンドを 1 行だけ」）にも
  同じ値が使われる。
- **シェル側 client は起動時に固定**。`AI_CHAT_API_KEY` / `AI_CHAT_MODEL` /
  `AI_CHAT_BASE_URL` / `AI_CHAT_TIMEOUT_SECS` を後から変えても `!` チャットにしか届かない。
  起動時にキーが無いと `integration_state.ai_service` は `None` のままで、後から設定しても
  コマンドパレットと `Alt+d` は「未設定」と言い続ける。`AI_MESSAGE_LANG` だけは
  `refresh_derived_state` が押し出す。
- **API キー未設定メッセージが 10 通り以上**。`AI_CHAT_API_KEY` だけ挙げるもの、
  `OPENAI_API_KEY` だけ挙げるもの、両方を逆順で挙げるものが混在している。
- **`dsh/src/ai_features/cache.rs` が `std::env::var` 直読み**。キャッシュキーが
  シェル変数のモデル/言語を見ないので、`(vset AI_CHAT_MODEL ...)` の直後 60 秒は
  旧モデルの答えが返る（TTL がその緩和策）。

### プロバイダ API（調査済み・未着手）

- **既定モデルでは temperature が効かない**。`client.rs` の
  `FIXED_TEMPERATURE_MODEL_PREFIXES`（`gpt-5` / `o1` / `o3` / `o4`）に当たると 1.0 を強制する。
  既定は `gpt-5-mini` なので、ゴーストテキストの 0.0 も JSON 生成の 0.1 も**すべて 1.0**。
  決定性が要るなら `reasoning_effort` を送る口を作るのが筋で、temperature を足しても意味がない。
- **`reasoning_effort` / `verbosity` を送る口が無い**。
- **構造化出力が `json_object` 止まり**。`json_schema` + `strict` にすれば
  `strip_code_fence` → `serde_json::from_str` → フォールバックの手作業が要らなくなる。
  対象は `safe_run` ×2、`ai_features/command.rs` ×3、comp-gen。
- **ストリーミングは経路 A（`!` チャット）だけ**。`ChatGptClient::send_chat_streaming` +
  `dsh_openai::stream` が SSE を非ストリーム同形の `Value` に集約し、`dsh-builtin/src/chatgpt.rs`
  の `StreamSink` が確定 Markdown ブロックを `dsh-builtin/src/markdown/stream.rs` の
  `MarkdownBlockSplitter` で切り出して逐次描画する。既定 ON、`AI_CHAT_STREAM=0` で無効化。
  経路 B（`dsh/src/ai_features/service.rs` / `AiService`）は非対応のまま
  （呼び出し元が 15 箇所以上あり、`-> Result<String>` を変える範囲が別作業になるため）。
  `safe_run` / `ai-commit` / ゴーストテキストは JSON か 1 行の最終値なので対象外。
- **リトライに jitter が無い**（`MAX_RETRIES=3`、500ms base、8s cap、`Retry-After` 尊重、
  タイムアウトは再試行しない）。
- **トークン見積もりがバイト長**。tokenizer は入っていない（`usage` ブロックは正確）。
  `truncate_middle` の予算も文字数ではなくバイト数なので、日本語では実効が約 1/3。

### MCP（調査済み・未着手）

- **永続接続が無い**。`list_tools` も `call_tool` も毎回接続して切る。stdio サーバは
  ツール呼び出しごとにプロセスを起動する。
- **`call_tool` のタイムアウトが 30 秒固定**、`list_tools` は**タイムアウト無し**。
- **ツールキャッシュに TTL が無い**。`ToolCacheEntry.timestamp` は書かれるだけで読まれない。
- **`unique_name` の連番が登録順に依存**する。リロードで同じツールの function 名が
  変わりうる（`mcp:<function_name>:<args>` のセッション承認がそこで無効化される）。

### 命名（直さない）

- **プロバイダ名入りの命名**が残っている。crate `dsh-openai`、module `chatgpt`、
  `openai-execute-tool.json`。実体は OpenAI 互換 API 全般。改名は互換を壊す。
- **builtin 名の表記ゆれ**。`chat_prompt` / `chat_model` / `chat_reset`（snake）と
  `ai-commit` / `ai-watch` / `safe-run`（kebab）。
