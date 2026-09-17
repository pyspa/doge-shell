# AI 機能の設計方針

doge-shell が**製品として持つ** AI 機能の方針。`docs/ai/` の他の文書は「この repo を AI に
編集させるときの運用ルール」で、別物。ここは実装の話をする。

新しい AI 機能を足すとき、既存の AI 機能を直すときに最初に読む。

このファイルは概論・プロバイダ・エージェントループ・再実装禁止事項・既定 OFF だけを持つ。
話題別の詳細は以下に分割してある:

| 話題 | ファイル |
|---|---|
| 安全ゲート（`SafetyGuard`） | [safety.md](safety.md) |
| 環境変数の正典 | [env-vars.md](env-vars.md) |
| Skill | [skill.md](skill.md) |
| AI chat hooks | [hooks.md](hooks.md) |
| 未解決の設計判断 | [open-questions.md](open-questions.md) |
| Herdr 連携・永続タスク (`agent`) | [herdr.md](herdr.md) |

親ファイル [../ai-architecture.md](../ai-architecture.md) はこの索引を短くまとめたものへのリンクだけを持つ。

## 1. プロバイダは OpenAI 互換 chat/completions のみ

- エンドポイントは `{base_url}/chat/completions` 固定（`dsh-openai/src/config.rs`）。
- 認証は `Authorization: Bearer` 固定。
- したがって **Anthropic Messages API（`x-api-key` + `anthropic-version`）はそのままでは使えない**。
  OpenAI 互換ゲートウェイ経由で使う。Responses API も対象外。
- **`is_openai_reasoning_model`（`OPENAI_REASONING_MODEL_PREFIXES` = `gpt-5` / `o1` / `o3` / `o4`、
  大文字小文字を無視、`client.rs`。`pub` で `dsh-openai` から公開）に該当するモデルへの `tools` 付き
  リクエストは、`AI_CHAT_REASONING_EFFORT` 未設定でも `reasoning_effort: "none"` を最初から送る**
  （`ChatGptClient::resolve_reasoning_effort`）。この lineup 全体が chat/completions で
  「`tools` + `none` 以外の `reasoning_effort`」を拒否すると確認済みなので、400 を待たず先回りする。
  既定モデル `gpt-5-mini` を含むため、これで設定ゼロで `!` チャットが動く。operator が明示的に値を
  設定した場合はまずそちらを試し、対立したら下の 400 リカバリで補正する（既に設定済みの値を無視して
  黙って `none` に上書きしない）。`tools` を送らないリクエスト（要約・`safe_run` の JSON 生成）は
  この既定の対象外で、モデル自身の既定 reasoning に任せる。未知のモデルやローカル / 互換サーバには
  一切影響しない — フィールドを送らない今までの挙動のまま。`is_openai_reasoning_model` は temperature
  強制（`final_temperature`）とこのデフォルトの両方が読む唯一の判定関数 — 2 つの制約が今日たまたま
  同じモデル集合に効くだけで、将来分岐したら 2 本目の prefix リストを足す（関数を分けない）。
  `doctor`（`dsh-builtin/src/doctor.rs`）もこの関数を呼んで表示を出す。プレフィックス表を
  ハードコードで再現しない。
- ローカル / 互換サーバ対応は 2 つの仕組みで済ませる。増やさない。
  - 400 リカバリ（`client.rs`）: `recover()` が 2 方向を試す。
    1. `reasoning_effort_conflict`: `tools` 付きリクエストが「`reasoning_effort` が `tools` と
       両立しない」400 を受けたら `reasoning_effort: "none"` を**足して**再送し、
       以後そのクライアントの `tools` 付きリクエストには常に `"none"` を強制する
       （`remember_reasoning_none_forced` / `reasoning_none_forced`。`tools` を送らないリクエストは影響を受けない）。
    2. `unsupported_field`（`DROPPABLE_FIELDS`）: 400 で拒否された optional フィールドを 1 回落として
       再送し、以後そのクライアントでは送らない。
    両者は同じ `RecoveryState` を共有し、**1 の判定を 2 より先に評価する**。逆にすると
    `reasoning_effort` が `DROPPABLE_FIELDS` にも入っているため 2 が先に一致して「落として」しまい、
    サーバ側既定が `none` 以外のまま同じ 400 が返って手詰まりになる。
  - `AI_CHAT_ALLOW_INSECURE_HTTP`: `http://` の base URL を許可する。既定では https へ差し替え、
    stderr に 1 回警告する。

## 2. エージェントループは 2 つだけ

| | A: `!` チャット | B: シェル側 AI |
|---|---|---|
| 入口 | `dsh/src/shell/eval.rs` → `dsh-builtin/src/chatgpt.rs` | `dsh/src/ai_features/service.rs` |
| 実行 | 同期 | 非同期 |
| ツール | builtin 10 種（`cron_manage` 含む）+ `job_status`/`job_output`/`job_cancel` + MCP | MCP 用の実行ループは持つが、本番の呼び出し元は全て `without_tools()` で opt-out しており実際には未使用 |
| 反復上限 | `MAX_TOOL_ITERATIONS` (100) | `MAX_ASSIST_ITERATIONS` (10) |

3 つ目を作らない。単発リクエスト（`ai-commit` / `safe-run` / ゴーストテキスト）は
ループを持たず、`turn::answer_text` で応答を読む。cron の各 run は
`dsh -c "cron run-job <id>"` の別プロセスで走り、子プロセスの起動は
`dsh/src/detached_child.rs` を共有する。

**コマンドは両経路とも managed job として走る**（`dsh-builtin/src/agent/jobs.rs` の
`AgentJobs`）。タスクのジョブは `AgentRuntime` が持ち SQLite に artifact を残す。経路 A の
ジョブは `dsh-builtin/src/chatgpt/jobs.rs` のプロセス内レジストリが持ち、**ターンを跨いで
残りうる**。`execute` が待つ時間は `AI_CHAT_EXECUTE_YIELD_MS`。

触るときの不変条件:

- **対話レジストリは経路 A だけのもの**。`chat_with_tools` の job 呼び出しは全て
  `setup.runtime.is_none()` の内側に置く。epilogue は両経路で共有されているので、外に出すと
  失敗した `agent run` が対話の `!` チャットのビルドを SIGKILL する。タスクの `job_status` は
  `runtime.jobs` を引くので、対話ジョブの id を prompt に載せても解決できない。
- **ジョブを残してよいのは「後のターンが名前を呼べるとき」だけ**。判定は
  `session_ttl.is_some() && (outcome.is_ok() || rewound)` — `store` は ttl が無いと no-op なので、
  outcome だけ見ると `AI_CHAT_SESSION_TTL_SECS=0` でポーリング不能なジョブが残る。
  片付けは `cancel_session`（自分の会話分だけ）で、`cancel_all` は `chat_reset` と shutdown 用。
- **モデルに渡す id は短縮しない**。`AgentJobs` は完全一致で引く。`describe_running` は人向けの
  短縮 id、`carried_notice` は完全な id、という対で保つ。
- **ライブ出力は `echo_pending` 1 本**（`chatgpt/jobs.rs`）。`execute` の待ちループと
  `job_status`/`job_output` のポーリングの両方から呼ぶ。片方だけにすると、yield を超えた
  ジョブの出力が画面に出なくなる。

`dsh-builtin/src/chatgpt/reflect.rs`（ターン末の skill リフレクション、既定 OFF）も単発リクエストの
一種。3 つ目のループではない根拠は 4 点: (1) `tools` を送らない → `tool_calls` が返り得ないので
ディスパッチも反復状態機械も存在しない、(2) `send_chat` を 1 回だけ呼ぶ（リトライ・再送・自前の
iteration 上限を持たない）、(3) 応答解釈は `turn::answer_text`（`interpret_response` は呼ばない）、
(4) 会話を作らない — `ConversationManager` を新規作成せず `manager.buffer` に書かず
`session::store` にも渡さない。副作用は `skills::pending` への提案 1 件だけで、`skill approve` を
人が打つまで誰もそのファイルを読まない。呼び出し位置は `chat_with_tools` の `'agent: loop` を抜けた
直後、`runtime.checkpoint`/`finish` より前（リフレクションのトークンをそのターン・タスクの集計に
含めるため）。失敗は握り潰して dim 1 行のみ、`outcome` を変えない。

両者が守る方針は `dsh-openai/src/turn.rs` に置く。ここに無い方針を片方だけに書かない。

AI chat hooks（`dsh-builtin/src/chatgpt/hooks/`）は経路 A だけに掛かる。`turn.rs` に置かないのは、
そこが「両経路が守る方針」の置き場所だから。経路 B へ広げるときに初めて移動を検討する。

## 3. 再実装してはいけないもの

| やりたいこと | 正の置き場所 |
|---|---|
| API 設定の解決（キー / モデル / base URL / timeout） | `OpenAiConfig::from_getter`（builtin からは `chatgpt::load_openai_config`） |
| 応答の解釈（tool_calls / answer / `finish_reason` / stall） | `dsh_openai::turn::{interpret_response, answer_text}` |
| 長い出力の切り詰め | `dsh_openai::turn::truncate_middle` |
| 応答言語の指示 | `dsh_openai::apply_language` |
| config ディレクトリ / skills ディレクトリ | `dsh_builtin::config_paths`（`dsh` crate 内は `environment::get_config_file`） |
| skill root の解決（user / project の順） | `dsh_builtin::chatgpt::skills::skill_roots` |
| project root の判定 | `chatgpt::tool::workspace_root` + `project_context::has_project_marker` |
| 秘密のマスキング | `dsh_types::safety_policy::redact_sensitive_text` / `chatgpt::tool::redact_tool_arguments` |
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

## 5. AI 機能は既定 OFF

`dsh/src/suggestion.rs` の既定は `ai_backfill: false` / `auto_fix: false` /
`ai_explanation: false`。API キーがあるだけでは何も自動で走らない。
有効化は `config.lisp` の `(set-suggestion-ai-enabled t)` /
`(set-auto-fix-enabled t)` / `(pref-ai-explanation t)`。

`!` チャット、`Alt+d`、`aic`、`safe-run`、`ai-watch` は明示的な操作なので、この既定とは無関係。
