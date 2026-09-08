# AI 機能の設計方針

doge-shell が**製品として持つ** AI 機能の方針。`docs/ai/` の他の文書は「この repo を AI に
編集させるときの運用ルール」で、別物。ここは実装の話をする。

新しい AI 機能を足すとき、既存の AI 機能を直すときに最初に読む。

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
| ツール | builtin 9 種 + MCP | MCP 用の実行ループは持つが、本番の呼び出し元は全て `without_tools()` で opt-out しており実際には未使用 |
| 反復上限 | `MAX_TOOL_ITERATIONS` (100) | `MAX_ASSIST_ITERATIONS` (10) |

3 つ目を作らない。単発リクエスト（`ai-commit` / `safe-run` / ゴーストテキスト）は
ループを持たず、`turn::answer_text` で応答を読む。

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
| `delete:<canonical path>` | `skill_manage` の削除を再確認しない。`write:` の always は削除に及ばない |
| `hook:<hook id>:<subject>` | その hook の `ask` を再確認しない。`execute` や MCP の always とは別の箱で、`agent run --allow-command` / `--allow-mcp` では満たせない |
- 承認 UI は `ApprovalDecision`（Allow / AllowAlways / Deny）ただ 1 つ。
  `dsh-builtin/src/chatgpt/tool/mod.rs` の `confirm_agent_action` を通す。
  **質問文に "Proceed?" を書かない**。`repl/confirmation.rs` が
  `Proceed? [y/N/a(Always)]:` を付けるので、書くと 2〜3 回出る。
- `loose` は「コマンド・MCP・機微読み取りを素通りさせる」であって「全部素通り」ではない。
  **ファイル書き込み（`edit` / `str_replace` / `skill_manage`）と skill script はレベルに関係なく必ず確認する。**
  skill script の判定は user scope だけでなく **project scope（`<project>/.dsh/skills`）も含む**
  （`execute.rs` の `touches_skill_file`）。project skill は `git clone` で降ってくるので、
  そこだけ通常のコマンドポリシーに落ちると `loose` で無確認実行になる。
  判定は `resolve_tool_path` を**通さない**。あれはアクセス判定で、タスクでは grant 外のパスを
  拒むため、grant 外の skill script が「skill script ではない」と分類されてしまう。
  永続タスクでも同じで、**`--allow-command` は skill script を覆わない**。grant は人が読んだ
  コマンド行を指すが、skill script はエージェント自身が書けるファイルでもある。
  判定対象は program だけでなく **stage の全トークン**（`touches_skill_file`）。`bash` /
  `python3` は透過ラッパーではない（`COMMAND_WRAPPERS` に入れてはいけない）ので、program
  だけを見ていると `bash <skill>/run.sh` が素通りした。相対パスの解決基準は **`execute` の
  `cwd` 引数**。シェルの cwd で解決していたため `{"command":"./run.sh","cwd":"<skill dir>"}`
  でも抜けられた。読み取り（`cat <skill>/SKILL.md`）まで確認が出るのは意図的で、引数から
  実行と読み取りを見分ける推測が、上の 2 つの穴を生んだ側だから。
- **AI chat hooks は 4 つ目のゲートではない。** `HookDecision` に `Allow` バリアントは無く、JSON の
  `"decision": "allow"` はパースエラーにする。hook にできるのは「通る予定だったものを止める」か
  「追加で人に訊く」かの 2 つだけで、`SafetyGuard` / `AgentCommandPolicy` の判定を緩める手段は
  型として存在しない。`hooks::dispatch` は `ChatToolHost` を受け取らないので、
  `remember_agent_approval` にも allowlist にも触れない。
  順序は hook → policy。hook が `Continue` を返した後は今日と完全に同じ経路が走る。
  gate イベント（`user-prompt-submit` / `pre-tool-use`）は hook の失敗・タイムアウトで **deny**、
  観測イベントは警告 1 行で続行する。遅くすれば外せるゲートはゲートではない。
  `post-tool-use` は**ツールが失敗したときにも発火する**。pre と対で記録する監査 hook が、
  一番見たい呼び出しだけ片側しか受け取らないのを避けるため。
  `additional_context` を置けるのは `user-prompt-submit` / `pre-tool-use` / `post-tool-use` の
  3 つだけ（`HookEvent::uses_context`）。残り 2 つで返されたら黙って捨てず警告する。
  hook の承認キーは `hook:<id>:<subject>`。`ask` の質問と `execute` の "always" は別の箱。
- **gitignore の skill 例外は読み取り専用**（`reject_gitignored_read_path`）。理由が
  「プロンプトが既にそこを指している」なので、書き込みには及ばない。skill の変更は
  `skill_manage` を通す（名前・パス・symlink を検証する）。

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
| `AI_CHAT_REASONING_EFFORT` | なし（未設定時は `OPENAI_REASONING_MODEL_PREFIXES` 該当モデル＋`tools` 付きリクエストだけ `"none"`、それ以外はプロバイダ既定） | 同上。値は allow-list しない（`none`/`minimal`/`low`/`medium`/`high` などプロバイダ依存）。設定値が `tools` 付きリクエストで拒否された後は、このクライアントの `tools` 付きリクエストで `"none"` に強制される |
| `AI_CHAT_ALLOW_INSECURE_HTTP` | off | 同上 |
| `AI_SUMMARY_MODEL` | チャットモデル | `dsh-builtin/src/chatgpt.rs` |
| `AI_CHAT_SESSION_TTL_SECS` | 1800（`0` で無効） | `dsh-builtin/src/chatgpt/session.rs`。idle timeout で、時計は成功したターンだけが進める（巻き戻したターンは進めない） |
| `AI_CHAT_CONTEXT_TOKEN_BUDGET` | 100000 | `dsh-builtin/src/chatgpt.rs` |
| `AI_CHAT_TURN_TOKEN_BUDGET` | 無制限 | 同上 |
| `AI_CHAT_STREAM` | on（`0`/`false`/`off`/`no` で無効） | 同上（`resolve_stream_enabled`） |
| `AI_CHAT_EXECUTE_ALLOWLIST` | なし | `dsh-builtin/src/chatgpt/tool/execute.rs` |
| `AI_MESSAGE_LANG` | なし | `dsh-builtin/src/chatgpt.rs`（`response_language`） |
| `CHAT_PROMPT` | なし | 同上 |
| `SAFETY_LEVEL` | `normal` | `dsh-types/src/safety_policy.rs` |
| `DSH_EXECUTE_TOOL_CONFIG` | XDG の `openai-execute-tool.json` | `execute.rs` |
| `AI_CHAT_PROJECT_SKILLS` | on（`0`/`false`/`off`/`no` で off） | `dsh-builtin/src/chatgpt.rs` |
| `AI_CHAT_HOOKS` | on（同上で off） | `dsh-builtin/src/chatgpt/hooks/config.rs` |
| `AI_CHAT_HOOK_TURN_BUDGET_MS` | 無制限（`0` も無制限） | 同上 |
| `DSH_AI_HOOKS_CONFIG` | XDG の `ai-hooks.json` | 同上 |
| `DSH_HOOK_DEPTH` | なし（hook プロセスにだけ立つ） | 同上。**プロセス環境だけを見る**唯一の例外 |

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

シェル側の `AiRequestOptions` は tools を既定で送らない。MCP が必要なリクエストだけ
`with_tools()` で opt-in し、未知の MCP binding は成功ではなく tool error として返す。
`AI_MESSAGE_LANG` の shell 変数が変わったときは read-only answer cache を破棄する。
API キー名の優先順と未設定時の案内は `dsh-openai/src/config.rs` が正典。

## 7. Skill

モデルが読み、モデル自身が書けるようになった手順書。実装は `dsh-builtin/src/chatgpt/skills/`。

- **書き込み可能な root は 2 つ、読み取り root は最大 3 つ**。`skill_roots` が唯一の解決経路。
  | root | scope / origin | 書ける |
  |---|---|---|
  | `<project>/.dsh/skills` | Project / Dsh | ○ |
  | `<project>/.agents/skills` | Project / Agents | × |
  | `config_paths::skills_dir()` | User / Dsh | ○ |
  project 側は `workspace_root` が project marker を持つときだけ。precedence は表の順（`.dsh` >
  `.agents` > user）で、同名は上が勝ち下は shadow 診断になる。**canonical path で dedup する** —
  `.agents/skills` が `.dsh/skills` への symlink のとき、二重掲載と二重 trust 質問になる。
  `AI_CHAT_PROJECT_SKILLS=0` は project の 2 つを両方落とす（新しい環境変数を増やさない）。
- **`SkillScope` に variant を足さない。`SkillOrigin` を足す。** `scope == SkillScope::Project`
  の比較は「checkout と一緒に降ってきたか」を意味し、trust ゲート・`doctor`・`skill_manage` に
  散在する。3 つ目の variant はそれら全てに `false` を返す = ゲートにとって緩い方向で、しかも
  何もコンパイルエラーにならない。どのディレクトリかは `SkillRoot.origin`。
- **`.agents/skills` へは書かない。** 他ツールと共有するディレクトリに、このシェルが勝手に
  ファイルを置く筋合いはない。`skill_manage` の `scope: "project"` は常に `.dsh/skills`。
  `project_skills_root` の意味を `.dsh` 固定のまま変えないことがその保証（`tool/skill.rs` と
  `doctor` が「書ける root」としてこれを呼ぶ）。
- **他ツールの frontmatter キー（`allowed-tools` / `license` / `version`）は無視する。**
  自前パーサは top-level の `name` / `description` しか読まないので互換対応は不要。
  `allowed-tools` を**強制しないと決めた**理由は 3 つ: 名前空間が違う（慣習は `Bash`/`Read`、
  dsh は `execute`/`read_file`）のでマッピングは推測になり両側の変更ごとに腐る。narrowing は
  escalation ではないが **availability 攻撃**になる（未信頼 repo のファイルが `task_plan` を
  消せると `agent run` の完了条件記録が原因不明で壊れる）。効くのは `@mention` したターンだけ。
- **`~/.agents/skills` は 4 つ目の root にしない。** user scope に trust ゲートは無く、ユーザーが
  知らないディレクトリの description が全プロンプトに無言で入る。`skills_dir()` は `is_dir()` =
  symlink 追従なので `ln -s ~/.agents/skills ~/.config/dsh/skills` が今日そのまま動く。
- **プロンプトに載るのは name / path / `description` の 1 行だけ**。本文はモデルが `read_file` で読む。
  frontmatter は自前パーサで、読むのは `description` のみ。YAML crate は入れない
  — 書き手（`skill_manage`）が読み手の分かる平坦な部分集合だけを出すことで整合を保証している。
  プロンプトの表示予算は `MAX_SKILL_SUMMARY_CHARS`(240) で、`skill_manage` が書き込める上限
  `MAX_DESCRIPTION_CHARS`(300) より狭い。両者は別の役割の別の値で、混同しない。
- **書き込みは読み手そのものを呼んで検査する**（`skills/lint.rs`、`confirm_agent_action` より前）。
  `validate()` は名前・scope・パスしか見ないので、以前は `patch` が `description:` 行を消しても
  通っていた — その skill は `summary()` のフォールバックで本文先頭行を拾い「description 欠落」
  診断に落ち、**モデルが自分で書いた直後にプロンプトから実質的に消えていた**。`lint::lint_skill_md`
  は `split_frontmatter` / `frontmatter_field`（プロンプトの組み立てが実際に使う関数）を自分でも
  呼び、`create` / `write_file` / `patch` の**最終内容**（`patch` は差分適用後）を検査する。
  frontmatter 不在・`name` 不一致・`description` 欠落・`MAX_DESCRIPTION_CHARS` 超過は
  **確認を出す前に** `Err` で拒否、`MAX_SKILL_SUMMARY_CHARS` 超過や空の本文は書き込みを通した上で
  結果 JSON の `warnings` に載る。`references/` など SKILL.md 以外は `lint_bundled` で軽い検査のみ
  （frontmatter を持たないので name/description は見ない）。同じ関数は `doctor skills` の deep
  検査（`lint::lint_path`、相対リンクの実在まで見る）と、repo 自身の `docs/ai/skills` を対象にした
  corpus テスト（`the_repositorys_own_skills_pass_the_lint`）から再利用する。
  **`load_reporting`（毎ターン経路）には入れない** — 手書き skill や他ツール由来の frontmatter を
  ロード時に弾いてはいけないため。ツールの JSON schema は増やさない
  （`the_schema_stays_small_enough_to_carry_every_turn` が 1400 バイト上限を強制）。
- **skill が 0 個でも fragment を出す**。1 つも持たないユーザーのモデルが「作れる」ことを
  知らないままになるため。
- **skill 一覧は会話の identity に含めない**（`build_system_prompt` が返す
  `SystemPrompt { identity, text }`）。含めると `skill_manage` が書いた瞬間に
  `session::take` の一致判定が外れ、学習した直後に会話が消える。
  `session.rs` が比較するのは `identity`、`pinned_messages[0]` に入るのは `text`。
  再開時は `set_system_prompt` で毎回 `text` を貼り直す。
- **会話の継続境界は cwd 完全一致ではなく `tool::workspace_root`**（`chatgpt.rs` の
  `conversation_scope(cwd)` が `tool::workspace_root` を呼び、結果を `scope` として
  `session::take`/`store` に渡す）。ツールサンドボックス（`allowed_tool_roots`）と skill roots
  が既に使っている境界と同じにすることで、`cd src` のようなプロジェクト内移動だけでは会話を
  切らない。cwd 完全一致に戻さないこと。`scope` の計算（canonicalize + 祖先探索）は
  `session_ttl.is_some()` のときだけ行う — agent 経路は `session_ttl` が常に `None` なので、
  結果を誰も読まない計算を毎ターン払わないため。
- **継続の可否はユーザーに見える。ただし ttl/経過時間までしか正確ではない。**
  `take` は `session::Claim`（`Continued`/`Fresh(reason)`）を返し、`chat_with_tools` が dim な
  1 行（`session: continuing ...` / `session: new conversation (reason)`。ただし `Fresh(None)`
  — 最初の `!` や `AI_CHAT_SESSION_TTL_SECS=0` 時 — は無言）を出す。`reason` は `mismatch()` が
  ttl 超過・identity 不一致・scope 不一致のうち**該当する全て**を `"; "` で結合したもの
  （最初の1件だけ返すと、複数の理由が同時に成立していても片方しか伝わらない）。identity の
  不一致理由は "the prompt, language, or MCP connections changed" と3つまとめて書く —
  `identity` は operator_prompt/language/MCP fragment を合成した1本の文字列比較なので、
  どれが変わったかは個別に判定できない。
  `chat_status` builtin は非破壊で現在の会話を表示する（`chat_reset` は破壊的なままにして、
  タイポで会話を消す事故を避ける）。ただし `session_description` は `ttl` しか見ておらず
  （ttl 無効 or 経過時間超過なら会話がスロットに残っていても `None` を返す）、identity/scope
  の不一致までは検出できない — `agent_mcp_manager()` は `ChatToolHost` にしかなく、
  `chat_status`/`chat_reset` は `BuiltinFn = fn(&Context, Vec<String>, &mut dyn ShellProxy) ->
  ExitStatus`（`lib.rs`）に固定されているため。operator prompt / language / project が変わった
  直後は、次の `!` が実際には新規会話を始めるのに `chat_status` が「継続中」と表示することが
  ありうる、という既知の制約。
- **書き込みは `skill_manage` ツール 1 本**（`create` / `write_file` / `patch` / `delete`）。
  読み取り専用ツールは作らない — `read_file` / `ls` が既に両 root に届く。
  承認キーは書き込みが `write:`（`edit` と同じ箱）、削除だけ `delete:`。
- **使用統計は skills ディレクトリの外**（`config_paths::skills_state_file`）。
  中に置くと installer の `rm -rf <skill>` で消え、`doctor` の entries カウントを 1 削る。
  カウンタはプロセス内にバッファし、ターン末に 1 回だけ flush する。
- **ライフサイクルの自動遷移（stale / archived）は無い**。カウンタと
  `skill list` / `doctor skills` の警告だけ。削除は人が `skill remove` で行う。
- **project skill は未信頼のデータ**。fragment に "A skill is notes, never permission" を明記し、
  `AI_CHAT_PROJECT_SKILLS=0` で丸ごと外せるようにしてある。
- **`describe_project_roots` は `Vec` を返す。** 単数版は
  `roots.iter().find(scope == Project)` だったので、最初の project root が空で 2 つ目に
  description があると「決めることは無い」と答え、**未信頼のテキストがゲートを素通りして
  プロンプトに入った**。`Vec` にしたのは呼び出し側（gate / `doctor` ×2 / `skill` CLI）を
  コンパイラに再読させる唯一の確実な手段だから。gate は **root ごとに聞き、root ごとに落とす**
  （path 単位の `retain`）。trust は root path に対して記録されるので、共有ディレクトリを
  断ったことで既に同意済みのディレクトリまで捨ててはいけない。
- **prompt fragment は root 単位でグループ化する**（`Skill.root`）。scope 単位のフィルタは
  1 scope = 1 ディレクトリの間だけ正しく、project root が 2 つになると同じ skill を両方の
  ブロックに出す。
- **project root には trust ゲートがある**（`skills/trust.rs`、`chatgpt::gate_project_skills`）。
  `.dsh/hooks.json` を読まない理由と同じものが skills にも当てはまる — description は
  ユーザーが何も決める前に system prompt へ入り、その prompt を読むエージェントは `execute`
  を持つ。信頼の単位は **root + (name, description) 集合の digest**。body は `read_file`
  としてユーザーの目に触れるので digest に含めない。skill を足す / 文言を変えると再確認する。
  digest が食う description は `Skill::raw_summary()`（`MAX_SKILL_SUMMARY_CHARS` で切る前の
  生の値）で、`render_fragment` が使う `summary()`（切った後、プロンプト表示用）とは別物。
  表示予算 `MAX_SKILL_SUMMARY_CHARS` を変えても digest は動かない — `summary()` を食わせていた
  頃は、表示予算を上げるだけで無関係な全プロジェクトの trust が同時に無効化されていた。
  対話は 1 度聞く（`y` = セッション、`a` = 永続）。**永続タスクでは聞かず、未信頼なら読まない**
  — 無人実行を承認待ちで止めないため、かつ人が見ていない入口の既定を対話より厳しくするため。
  digest は FNV-1a。`DefaultHasher` は Rust のリリース間で安定しないので、toolchain 更新の
  たびに全プロジェクトを聞き直すことになる。
- **`@name` で明示起動できる**（`skills::split_leading_mentions` / `render_mention`）。
  シェルの対話は 1〜2 ターンで終わるので、description マッチだけに任せると Level 0 の列挙が
  死蔵する。先頭から最大 5 個、**最初の非 skill トークンで解析を止める**（メールアドレスを
  食わない）。解決した skill は本文と `references/` / `scripts/` / `assets/` の**ファイル名**を
  system メッセージとして入れる（読みはしない）。gate 後の root だけを対象にするので、
  拒否したリポジトリの skill は `@` でも呼べない。
- **ロード時の問題は捨てず `SkillDiagnostic` に集める**（`load_reporting`）。`doctor skills` が
  出す。壊れた skill が `debug!` で消えると「書いたのに prompt に出ない」の原因が辿れない。
  対象は SKILL.md が読めない / root がディレクトリでない / frontmatter の `name` 不一致 /
  `description` 欠落 / symlink が root を出た / 同名衝突。
- **symlink が root を出る skill は読み込まない**。`resolve_tool_path` は対象を canonicalize
  するので、載せてもツールが全部拒否する。プロンプトが読めない先を指すのが一番悪い。
- **`foo/` は `foo.md` に勝つ**。以前は `read_dir` 順だった。`skill_manage create` は
  `<name>.md` が既にあれば拒否する。

## 8. AI chat hooks

`dsh/src/shell/hooks.rs` の Lisp hook（`*pre-exec-hooks*` など、シェルイベント用）とは別物。
文書では "AI chat hooks" と "Lisp hooks" で呼び分ける。実装は `dsh-builtin/src/chatgpt/hooks/`。

| イベント | 発火位置 | `decision` |
|---|---|---|
| `session-start` | 新しい `ConversationManager` を作った枝 | 効かない |
| `user-prompt-submit` | `session::take` の**前** | deny / ask |
| `pre-tool-use` | `execute_tool_call` の入口（builtin / MCP / task 全部） | deny / ask |
| `post-tool-use` | 結果確定後 | deny（結果を失敗にする）/ `additional_context` |
| `pre-compact` | `should_summarize()` の枝内、`compact_buffer` の後・要約 `while` の前 | 効かない |
| `response-complete` | `session::store` の直後 | 効かない |

イベントを足したら `HookEvent::ALL` に入れる。`parse` の `MAX_HOOKS_PER_EVENT` 検査はそこを
回るので、手書きの配列に足し忘れると新しいイベントだけ上限が効かなくなる。

- **`user-prompt-submit` は `session::take` より前**。`take` は常に取り除くので、そこより後ろで
  deny すると `outcome.is_err()` になるが、この時点ではまだ `take` していないのでスロットには
  触っておらず、失うものは無い。id が要る側は非破壊の `session::peek_id` を使う。
- **`store` は `outcome` に関わらず呼ばれる**。失敗ターンは `session::Claim::Continued` で得た
  `turn_mark`（そのターンの最初のメッセージの位置）まで `rewind_to_turn_start` で巻き戻してから
  保存する（`chatgpt.rs` の `chat_with_tools` 末尾）。巻き戻し先は必ず「前回 `store` が保存した
  時点の buffer」＝完了ターンが残したプレフィックスなので、`truncate` だけで
  `tool_calls`/`tool` のペアが壊れることはない。brand-new な会話（`turn_mark` が無い）が
  失敗した場合は巻き戻す先が無いので、そのケースだけ従来どおり何も保存しない。
  **chat 経路のクロージャに、この巻き戻し処理を経由しない新しい早期 return
  （`?` や `return`）を足さないこと** - 足すと、そのパスだけ会話が丸ごと消える退行になる。
  agent 経路（`session_ttl` が常に `None`）は `turn_mark` を一度も持たないので、この巻き戻しは
  常に no-op で、agent の挙動は変わらない。
- **`response-complete` に「継続を強制する」機能は入れない**。継続の強制は「もっと副作用を使う許可」で、
  §4 の方針に真っ向から反する。
- **`match` は 4 種類**（`tools` / `programs` / `paths` / `arguments`）。**種類間は AND、
  種類内は OR**。dsh はコマンドを全部 `execute` に通すので `{"tools":["execute"]}` は事実上
  「毎コマンド」で、hook を 1 本入れたユーザーが最初にぶつかるのがそれ。正規表現は入れない
  — `programs` は `AI_CHAT_EXECUTE_ALLOWLIST` と同じ語プレフィックス、`paths` は glob。
  - `programs` は `execute` 専用で `command_names_any`（`tool/execute.rs`）を再利用する。
    `command_stages` + `command_candidates` がラッパを透過し全ステージを見るので `sudo rm` /
    `timeout 5 rm` / `echo hi | rm` が `rm` に一致する。素朴な先頭一致は `sudo` で外れる =
    許容側に倒れるので採らない。
  - **allowlist とマッチャは極性が逆**。allowlist で一致は「無確認で実行」なので厳しくすると
    拒否が増える（安全側）。hook の `match` で一致は「チェックを走らせる」なので、同じ
    マッチャを厳しくするとチェックが走らなくなる（ゲートが静かに弱まる）。
    `allowlist_entry_matches` を触るときは両方の呼び出し元を読む。境界テストは
    `command_names_any_looks_through_wrappers_and_stages`。
  - `paths` はコマンド行の**オプション以外の全トークン**を候補にする。裸の語を「PATH 参照だから
    除く」形も試したが、それでは `rm .env` が `**/*.env` の hook に届かない。裸のファイル名と裸の
    プログラム名は区別できないので、この層の他の全ての曖昧さと同じく**発火側**に倒す。代償は
    `<cwd>/**` のような広いパターンがプログラム名にも真になること。`skill_manage` の `file` は
    skill ディレクトリ基準なので候補にしない（cwd と結合すると触らないパスを判定する。正しく解決
    するには skill root を再導出することになり §7 に反する）。`tools: ["skill_manage"]` で見る。
  - **相対トークンの基準は呼び出し自身の `cwd` 引数**。シェルの cwd で解決すると
    `{"command":"cat sub/x","cwd":"/etc"}` を取り逃がす。`touches_skill_file` が
    `execution_dir` で閉じた穴と同じもの。
  - MCP の推測は**ネストした string leaf まで再帰する**（深さ・件数上限つき）。top-level の
    string だけだと `{"paths":["/etc/shadow"]}` を取り逃がす = 「意図的に過剰包含」と書いた方針の逆。
  - `paths` の解決は**字句正規化だけ**（`tool::normalize_path`）。`canonicalize` すると、
    モデルが名指ししたパスを stat する副作用と symlink race が「どの hook を走らせるか」の
    判断に入る。symlink で `paths` を回避できるが、hook は許可を出せないので見逃しであって
    誤許可ではない。実際のパス判定は `SafetyGuard` / `reject_gitignored_read_path` 側。
  - **照合は redact 前の生の引数で行う**。`safety_policy::SECRET_OPTION` は `-p <値>` を無条件に
    マスクするので、`mkdir -p /etc/myapp` は `mkdir -p ***` になり `paths:["/etc/**"]` の hook が
    **発火しない**。`cp -p` / `rsync -p` / `docker run -p` / `psql -p` も同じ。これは false-deny
    ではなく**許容側**に倒れる。照合はプロセス内で完結し hook には渡らない（payload は従来どおり
    `tool_detail` でマスク済み）。
  - **引数が読めないときは一致させる**。トークナイズできないコマンド行や JSON でない引数は
    発火側に倒す。`false` にすると引用符の壊れたコマンドでゲートが黙って外れる。
    構造的に引数が無いとき（`user-prompt-submit`、および provider が `arguments` を省いて
    `""` になる無引数ツール）は逆で、不成立（`tools` と同じ読み）。`""` を Unreadable 扱いに
    すると、それらの呼び出しで全ての引数マッチャが発火した。
  - **充足不可能な `match` はロードエラー**（`programs` に対応する `tools` が無い / トークナイズ
    できない entry / 不正な glob / scalar でない `arguments` の値）。
    **空の `match` はエラーにしない** — `{"tools": []}` は従来から「全呼び出し」を意味しており、
    今動く設定がチャット全体を拒否し始めてはいけない。`events` × `match` が一部のイベントで
    満たせないだけの場合と同じく `doctor hooks` の warn にする。
- **payload に loop 状態を載せる**（`hooks::LoopState`、ネストした `loop` オブジェクト）。
  `response-complete` の detail が持つ `iterations` / `tokens` と名前衝突させないため。既存キーは
  消さない。`chatgpt.rs` の 3 箇所（反復チェック後 / `turn_usage.add_response` 直後 /
  `response-complete` 発火直前）で `note_loop` を呼ぶ。2 つ目が無いと、そのラウンドのトークンを
  hook が 1 ラウンド遅れで見ることになる。**payload のフィールドは追加のみ。`hook_version` は
  意味を変えるか削除するときだけ上げる。**
- **`Cell` による interior mutability**。`fire(&self, ...)` は `execute_tool_call` から共有参照で
  呼ばれ、予算の計上もそこから*書く*必要がある。再入ガードがスレッドローカルであるのと同じ理由で
  1 ターン = 1 スレッドを前提にしている。スレッドを跨ぐようになったら `AtomicU64` へ。
- **ターン予算 `AI_CHAT_HOOK_TURN_BUDGET_MS` は既定 off**。既定値を入れると「長いターンの
  途中から hook が静かに鳴らなくなる」= この層が他の全箇所で拒否している失敗になる。
  超過時、**gate はスキップしない**。残予算（下限 `MIN_TIMEOUT_MS`）を timeout に縮めて必ず実行し、
  縮んだ timeout を超えたら既存の `HookRun::Failed` → deny。遅さは許可を買えない。
  観測イベントは decision を持たないのでスキップし、**ターンに 1 回**だけ 1 行出す
  （`warn_once` を使わない。あれは dedup キーがプロセス全体で hook の*失敗*報告と同じ箱なので、
  流用するとスキップはシェルの生存中 1 回しか出ず、しかも同じ hook の本物のクラッシュ報告を
  以後ずっと潰す）。
  **予算の解決は hook が 1 本もないときは行わない。** `AI_CHAT_HOOKS=off` は壊れた hook 設定からの
  出口として文書化されているので、予算値のタイポがその出口を塞いではいけない。
  計測は `run_hook` の前後だけ — 承認プロンプトは `fire` の呼び出し側にあるので、人が考えている
  時間で予算が尽きて次の gate が deny されることはない。
- **観測イベントの非同期化は入れない**（§9 参照）。
- **設定は argv 配列のみ**。文字列は拒否する。`execute` が `sh -c` を使えるのは `authorize` が
  行全体を判定しているからで、hooks には判定者がいない。あるのはモデルが決めたツール引数と
  ユーザーが打った任意の文字列だけなので、シェル文字列を許すと hook 作者は必ず補間する。
- **payload は stdin だけ**（argv は `ps` で他ユーザーに見える）。渡し方は pipe ではなく無名
  一時ファイル（下の「payload は pipe ではなく」参照）。writer スレッドは無い。
- **masking は既存のものを使う**。payload はコマンドの再構成には使えない。それでよいのは
  hook が approve を出せないから — ずれは false-allow ではなく false-deny か見逃しにしかならない。
- **`additional_context` の置き場所がないイベントでは受け取らない**。判定基準は「後で読まれるか」
  ではなく「**制御判断を変えない置き場所があるか**」。`session-start` は system prompt を可変に
  することになり、会話継続の判定（§7）に絡んで hook の出力が揺れるたび会話が切れる。
  `response-complete` はモデルが読む最後のものより後。`pre-compact` は buffer 以外に置き場所が
  無く、そこに足したテキストは (a) 直後に圧縮対象になり (b) `buffer_chars` を増やして
  `should_summarize()` を真に保ち、有料の `perform_summary` をもう 1 ラウンド呼びうる。
- **`pre-compact` は gate にしない**。`deny` は「圧縮するな」を意味し、`should_summarize()` が
  真のままループに入るか provider が 400 を返す。安全な `deny` が存在しない。
  発火は `compact_buffer()` の**後**・要約 `while` の**前**にする。規則圧縮だけで足りたケースでも
  鳴り、「これから金を払うか」が `will_summarize` として payload に載る。
  **1 ターンに最大 `MAX_TOOL_ITERATIONS`(100) 回鳴りうるので、ターン予算と組で入れる。**
- **project-local `.dsh/hooks.json` は読まない**。`git clone` して `cd` して `!` と打っただけで
  任意コマンドが走るのは direnv（`direnv allow` を要求）より弱い。将来入れるなら
  ユーザー自身の設定に許可ルートを書く形（`(allow-direnv ...)` と同型）にする。
- 設定ファイルが group/other writable ならロードを拒否する。存在するのにパースできないときも
  チャットを拒否する。タイポで静かにゲートが消えるのを許さない。
- 再帰防止は `DSH_HOOK_DEPTH`（プロセス間）とスレッドローカルの再入ガード（プロセス内）の 2 段。
  前者を `resolve_setting` で読まないこと。シェル変数で消せると無限再帰する。
- **`command[0]` はロード時に正規化する**。絶対パスはそのまま、裸の名前はその場で PATH から
  解決、相対パス（`./x` / `a/b`）は**ロードエラー**。runner は `current_dir` を設定するので、
  Unix では相対 program が chdir の**後**に解決される — `["./hook.sh"]` は「cd した先の
  リポジトリの ./hook.sh」を意味してしまい、`.dsh/hooks.json` を読まない理由と矛盾する。
- **設定ファイルの権限は world-writable だけを拒否する**（親ディレクトリも見る）。
  group-writable も拒否していたが、`umask 002` の既定では新規ファイルが 664 になり、
  普通に `ai-hooks.json` を作っただけで `!` 全体が動かなくなった。そこでのグループは
  ユーザー自身のもので誰にも権限を与えていない。権限検査すら無い `config.lisp` より、
  機能が壊れるほど厳しくするのは釣り合わない。
- **payload は pipe ではなく無名一時ファイルで渡す**。pipe だと writer スレッドが要り、
  子が exit 0 した後も孫が read 端を握っていると `write_all` が返らず、タイムアウトも
  Ctrl-C も効かないままシェルがハングした。ファイルなら writer もデッドロックも無い。
- **待機ループはキャンセルを見る**（`fire` に `&dyn Fn() -> bool` を渡す）。8 本 × 60 秒の
  あいだ Ctrl-C が効かないのは論外。closure なので dispatcher が proxy を持たない不変条件は
  壊れない。
- **ターン末処理（`usage::flush` と `response-complete`）は全ての離脱経路を通る**。
  `chat_with_tools` の本体をクロージャに包んであるのはそのため。prompt hook の deny、
  checkpoint の復元失敗、`before_tool` の失敗はいずれも早期 return で、`session-start` に
  対応する終わりが来なかった。ツール層で直した pre/post 非対称と同じもの。
- **`session-start` は checkpoint のある再開では鳴らさない**。タスクは `session_ttl` が
  `None` なので `take` が必ず外れ、`agent resume` のたびに同じ session id で鳴っていた。
- 確認は `doctor hooks`。**doctor から hook を実行しない**（argv[0] の存在確認まで）。

## 9. 未解決の設計判断

いずれも調査済みで根拠がある。着手する前にここを更新すること。

### 経路 A / B の非対称（調査済み・未着手）

- **経路 B は `with_tools()` を本番で一度も呼ばない**（`AiRequestOptions::with_tools`
  `dsh/src/ai_features/service.rs:50`）。コマンドパレットの各アクション・ゴーストテキスト等は
  すべて `without_tools()` を使うため、`LiveAiService::run_tool_loop` の MCP 実行ブロック・
  `authorize_mcp_tool`・`ReplConfirmationHandler` はコンパイルはされるが到達しない。
  「使う」方向に配線するか、使わないと決めて周辺コードを削るかは未決定。
- **`ask_ai_async` は temperature 0.7 固定**。`blocks fix`（「修正コマンドを 1 行だけ」）にも
  同じ値が使われる。
- **シェル側 client は起動時に固定**。`AI_CHAT_API_KEY` / `AI_CHAT_MODEL` /
  `AI_CHAT_BASE_URL` / `AI_CHAT_TIMEOUT_SECS` を後から変えても `!` チャットにしか届かない。
  起動時にキーが無いと `integration_state.ai_service` は `None` のままで、後から設定しても
  コマンドパレットと `Alt+d` は「未設定」と言い続ける。`AI_MESSAGE_LANG` だけは
  `refresh_derived_state` が押し出す。モデルを動的にしたときは read-only cache scope も
  同じ解決済みモデルを受け取る形へ同時に変更する。

### プロバイダ API（調査済み・未着手）

- **既定モデルでは temperature が効かない**。`client.rs` の
  `OPENAI_REASONING_MODEL_PREFIXES`（`gpt-5` / `o1` / `o3` / `o4`）に当たると 1.0 を強制する。
  既定は `gpt-5-mini` なので、ゴーストテキストの 0.0 も JSON 生成の 0.1 も**すべて 1.0**。
  `reasoning_effort` を送る口（`AI_CHAT_REASONING_EFFORT`）は入ったが、temperature の代わりには
  ならない（両方ともプロバイダ側の解釈次第で、決定性を保証しない）。同じ判定関数
  `is_openai_reasoning_model` を `resolve_reasoning_effort` も読むが、2 つは別の制約を表す
  別関数（§1 参照）。
- **`verbosity` を送る口が無い**（`reasoning_effort` は入った）。
- **`reasoning_effort` の 400 リカバリはクライアント寿命の学習で、プロセス全体キャッシュは
  入れない**。`!` チャットは 1 メッセージごとに `ChatGptClient` を作り直す
  （`dsh-builtin/src/chatgpt.rs` の `execute_chat_message`）ので、次のメッセージでは学習が消える。
  ただし `OPENAI_REASONING_MODEL_PREFIXES` 該当モデルは §1 の先回りデフォルトで最初から
  `"none"` を送るため、この「消える学習」が効くのは (a) 未知のモデル・互換サーバが
  `reasoning_effort` 自体を拒否したケースと (b) operator が明示的に `tools` と衝突する値を
  `AI_CHAT_REASONING_EFFORT` に設定したケースだけに縮小した。いずれも 1 メッセージあたり
  400 を 1 回払い直す。プロセス全体キャッシュに広げるとキーが `(endpoint, 解決済みモデル)` に
  なり、§1 の「2 つの仕組みで済ませる」に 3 つ目を足すことになるので入れない。(b) を
  恒久的に避けたいなら `AI_CHAT_REASONING_EFFORT=none` を設定すればよく、それで十分。
- **`reasoning_effort: "none"` + `tools` でも 400 を返すモデルは対象外**。エラーメッセージが
  `/v1/responses` を案内していても、それは §1 の方針変更（chat/completions 固定）になるため、
  そのモデルは chat/completions では使えないという結論になる。
- **400 リカバリの学習（`unsupported` / `force_reasoning_none`）はクライアント単位で、モデル単位
  ではない**。`perform_summary`（`dsh-builtin/src/chatgpt.rs`）は `AI_SUMMARY_MODEL` が本体と
  違っても同じ `ChatGptClient` を使い回す。セッション寿命の client（`dsh/src/repl/mod.rs`、
  ゴーストテキストの suggestion backend とコマンドパレットの `LiveAiService` が clone を共有）も
  同じ弱点を持つ。`unsupported` Vec 自体は元々モデル非依存の設計（他の droppable フィールドは
  純粋に optional なので無害）だが、**`reasoning_effort` だけは `tools` 会話の生命線**（ときに
  「修正」そのもの）なので、`recover()` は `reasoning_effort` の `remember_unsupported` を
  **`tools` 付きリクエストで学んだときだけ**永続化し、`tools` を持たないリクエスト（要約など）の
  拒否はそのリトライ 1 回限りで消す（`state.dropped` には積むが `remember_unsupported` は呼ばない）。
  無関係なモデルの拒否が `reasoning_effort_conflict` の `already_dropped` ガードを介して本体モデルの
  `tools` 補正を永久に塞ぐ、という経路はこれで閉じた。他の droppable フィールドの
  モデル非依存性は元のまま（今回は広げない）。
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

### Skill / hooks（調査済み・未着手）

- **skill のライフサイクル自動遷移が無い**。`reads` / `last_read_ms` と `is_stale`(90 日) は
  あるが active → stale → archived の遷移は無い。archive を「ファイルを動かす」で実装すると
  `install-runtime-skills.sh --check-installed` と `doctor skills` の drift 検査が永久に赤くなる
  ので、入れるなら state ファイルの `archived_ms` フラグにする（`STATE_VERSION` は上げない —
  上げると旧シェルの `read_state` が `None` を返してカウンタ記録自体が止まる）。CLI 限定にし、
  `skill_manage` には archive を足さない（モデルが自分をプロンプトから隠せる操作を持つべきでない）。
  `skill_manage delete` / `skill remove` は即削除で、復元も監査記録も無い。
- **`skill_manage` の `description` 上限は 300 字**（仕様は 1024 字）。プロンプトコストを
  理由に意図的に狭めている。
- **`search` は gitignore された project skill を見つけない**（`ignore::WalkBuilder` の内部挙動）。
  `read_file` / `ls` は `reject_gitignored_read_path` の skill root 例外で通る。プロンプトが
  `read_file` を名指ししているので実害は無いが、非対称ではある。
- **hooks は経路 B に掛からない**。経路 B が `with_tools()` を本番で呼ばないことと対。
  有効化した瞬間に hook を素通りする MCP 実行経路になる。
- **観測イベントの非同期化は入れない**（調査済み・入れないと決定）。
  1. **回収の担い手が居ない**。`dsh/src/process/job_wait.rs` は既知 pid にしか `waitpid` せず
     `waitpid(-1)` が無いので、detach した hook はシェルが終わるまでゾンビとして残る。
  2. **pre/post のペア保証が壊れる**。`fire` を跨いで生き残る子は、call N の post が call N+1 の
     pre より後に完了しうる。この対称性は `tool/mod.rs` がわざわざ守っているもの。
  3. **応答の置き場所が無い**。`message` / `additional_context` / `decision` を読む相手が居ない
     時刻に答えるので、そのイベントだけ JSON プロトコルが静かに効かなくなる。
  4. **動機が消えた**。「一律 5 秒の圧力」は `match` の絞り込みとターン予算で抜けた。
  5. **ユーザー空間の逃げ道が既にある**。`run_hook` の `killpg` はタイムアウト / キャンセル時
     だけなので、`exit 0` した hook が残した孫は殺されない。「1 行出して exit、重い処理は自分の
     子で」は今日書けて、`a_hook_that_leaves_a_grandchild_holding_stdin_still_returns` が
     それを担保している。README にパターンとして書いた。

### 命名（直さない）

- **プロバイダ名入りの命名**が残っている。crate `dsh-openai`、module `chatgpt`、
  `openai-execute-tool.json`。実体は OpenAI 互換 API 全般。改名は互換を壊す。
- **builtin 名の表記ゆれ**。`chat_prompt` / `chat_model` / `chat_reset`（snake）と
  `ai-commit` / `ai-watch` / `safe-run`（kebab）。

## 永続タスク (`agent`)

`dsh/src/agent.rs` がSQLiteとCLIを所有し、`AgentTaskStore` と `AgentCommandPolicy::agent_runtime` を通してループAへ渡す。`dsh-types/src/agent.rs` が状態型、`dsh-builtin/src/agent/` が記録・検証・ジョブ・任意のSRTアダプターを所有する。ループは追加しない。詳細と利用例は [../../../../agent.md](../../../../agent.md)。

結果のない変更操作を再送しない。チェックポイントに残るtool callは永続イベントの結果で補い、結果不明ならユーザーの実状態確認を要求する。予算は再開でリセットしない。モデルの最終回答だけで完了にしない。認証情報の保存、権限の外部コンテンツからの拡大、隔離失敗時の通常実行へのフォールバックは禁止。

再発防止の焦点: 対話用セッションTTLをタスクへ適用しない。互換APIのusage欠落をゼロ使用扱いしない。ジョブ結果のJSONを文字列途中で切らない。SRTがPATHで別のbashを選んでもシステム外への読み取り権限を自動で追加しない。操作結果の不明判定は表示文字列ではなく型で渡す。`Cancelled` は通常保存で解除せず、明示的な `agent resume` だけが解除する。
