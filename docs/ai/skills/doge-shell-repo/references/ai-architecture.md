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
    （`printf ... | sh`）と `eval`。
- 分類は**行全体を対象に**。`Job.cmd` はパイプラインもまとめて 1 本の文字列なので、
  オペレータで区切り、ラッパー（`sudo` / `timeout` / `env` …）を覗いてから
  各段を分類する（`split_command_segments` + `command_candidates`）。
  先頭トークンだけを見ると `true | rm -rf ~` も `sudo rm -rf ~` も素通りする。
- allowlist は 3 種類あり、意味が違う。混ぜない。
  - 設定 allowlist（`(chat-execute-add ...)` / JSON / env）— **トークン前方一致**。人が書いた。
  - エージェントのセッション承認 — **完全一致**。`rm -rf target` の承認が
    `rm -rf target ~/documents` に及んではいけない。
  - ユーザー自身の "always"（`shell_always_allowlist`）— **AI には渡らない**。
    自分に許可したことは AI に許可したことではない。

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
| `AI_CHAT_EXECUTE_ALLOWLIST` | なし | `dsh-builtin/src/chatgpt/tool/execute.rs` |
| `AI_MESSAGE_LANG` | なし | `dsh-builtin/src/chatgpt.rs`（`response_language`） |
| `CHAT_PROMPT` | なし | 同上 |
| `SAFETY_LEVEL` | `normal` | `dsh-types/src/safety_policy.rs` |
| `DSH_EXECUTE_TOOL_CONFIG` | XDG の `openai-execute-tool.json` | `execute.rs` |

`AI_MESSAGE_LANG` は**散文にだけ**効く。JSON を返させるリクエスト
（`AiRequestOptions::json_object`）には付けない。フォーマットが壊れるうえ、
呼び出し側はフィールドを読むだけで文章を読まない。

モデル名の既定値は `dsh_openai::DEFAULT_MODEL` ただ 1 つ。
`doctor` を含め、どこにも文字列で書き写さない。

## 7. 未解決の設計判断

手を付ける前にここを更新すること。

- **承認 UI が 2 種類ある**。`execute` と MCP は `ApprovalDecision`（Allow / AllowAlways / Deny）
  の 3 値だが、`edit` / `str_replace` / `read_file` / `ls` / `search` は
  `ShellProxy::confirm_action` の bool のみで「always」が選べない。
  3 値化には "always" の記憶粒度（ファイル単位 / ツール単位 / 内容単位）を決める必要があり、
  これは一貫性の回復ではなく権限モデルの追加になる。
- **ストリーミング非対応**。`dsh-openai` は非ストリーミングのみ。最大 100 反復の実行が
  スピナーとツール名のログだけで進む。
- **構造化出力が `json_object` 止まり**。`json_schema` + `strict` にすれば
  パース失敗時のフォールバックが要らなくなる。
- **`reasoning_effort` / `verbosity` を送る口が無い**。既定モデルが reasoning 系なのに調整できない。
- **MCP に永続接続が無い**。`list_tools` も `call_tool` も毎回接続して切る。
  stdio サーバはツール呼び出しごとにプロセスを起動する。ツール呼び出しの
  タイムアウトは 30 秒固定で設定できない。
- **プロバイダ名入りの命名**が残っている。crate `dsh-openai`、module `chatgpt`、
  `openai-execute-tool.json`。実体は OpenAI 互換 API 全般。改名は互換を壊す。
- **builtin 名の表記ゆれ**。`chat_prompt` / `chat_model` / `chat_reset`（snake）と
  `ai-commit` / `ai-watch` / `safe-run`（kebab）。
