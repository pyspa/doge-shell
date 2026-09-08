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
  （`execute.rs` の `is_skill_script_program`）。project skill は `git clone` で降ってくるので、
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
| `AI_CHAT_PROJECT_SKILLS` | on（`0`/`false`/`off`/`no` で off） | `dsh-builtin/src/chatgpt.rs` |
| `AI_CHAT_HOOKS` | on（同上で off） | `dsh-builtin/src/chatgpt/hooks/config.rs` |
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

- **root は 2 つ**。`<project>/.dsh/skills`（`workspace_root` が project marker を持つときだけ）と
  `config_paths::skills_dir()`。同名は **project が勝つ**。`skill_roots` が唯一の解決経路。
- **プロンプトに載るのは name / path / `description` の 1 行だけ**。本文はモデルが `read_file` で読む。
  frontmatter は自前パーサで、読むのは `description` のみ。YAML crate は入れない
  — 書き手（`skill_manage`）が読み手の分かる平坦な部分集合だけを出すことで整合を保証している。
- **skill が 0 個でも fragment を出す**。1 つも持たないユーザーのモデルが「作れる」ことを
  知らないままになるため。
- **skill 一覧は会話の identity に含めない**（`build_system_prompt` が返す
  `SystemPrompt { identity, text }`）。含めると `skill_manage` が書いた瞬間に
  `session::take` の一致判定が外れ、学習した直後に会話が消える。
  `session.rs` が比較するのは `identity`、`pinned_messages[0]` に入るのは `text`。
  再開時は `set_system_prompt` で毎回 `text` を貼り直す。
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
- **project root には trust ゲートがある**（`skills/trust.rs`、`chatgpt::gate_project_skills`）。
  `.dsh/hooks.json` を読まない理由と同じものが skills にも当てはまる — description は
  ユーザーが何も決める前に system prompt へ入り、その prompt を読むエージェントは `execute`
  を持つ。信頼の単位は **root + (name, description) 集合の digest**。body は `read_file`
  としてユーザーの目に触れるので digest に含めない。skill を足す / 文言を変えると再確認する。
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
| `response-complete` | `session::store` の直後 | 効かない |

- **`user-prompt-submit` は `session::take` より前**。`take` は常に取り除くので、後ろで deny すると
  `outcome.is_ok()` が false になり `session::store` がスキップされ、継続中の会話が黙って消える。
  id が要る側は非破壊の `session::peek_id` を使う。
- **`response-complete` に「継続を強制する」機能は入れない**。継続の強制は「もっと副作用を使う許可」で、
  §4 の方針に真っ向から反する。
- **設定は argv 配列のみ**。文字列は拒否する。`execute` が `sh -c` を使えるのは `authorize` が
  行全体を判定しているからで、hooks には判定者がいない。あるのはモデルが決めたツール引数と
  ユーザーが打った任意の文字列だけなので、シェル文字列を許すと hook 作者は必ず補間する。
- **payload は stdin だけ**（argv は `ps` で他ユーザーに見える）。書き込みは別スレッドから行う
  — 同期書き込みしながら子の stdout が埋まると古典的なパイプデッドロックになる。
- **masking は既存のものを使う**。payload はコマンドの再構成には使えない。それでよいのは
  hook が approve を出せないから — ずれは false-allow ではなく false-deny か見逃しにしかならない。
- **`additional_context` の置き場所がないイベントでは受け取らない**。`session-start` は
  system prompt を可変にすることになり、会話継続の判定（§7）に絡んで hook の出力が揺れるたび
  会話が切れる。`response-complete` はモデルが読む最後のものより後。
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

### Skill / hooks（調査済み・未着手）

- **skill のライフサイクル自動遷移が無い**。`reads` / `last_read_ms` は記録するが、
  active → stale → archived の遷移は実装していない。archive は「ファイルを動かす」ことで、
  `install-runtime-skills.sh --check-installed` が drift を報告し続ける。走らせる常駐プロセスも無い。
  `skill_manage delete` / `skill remove` は即削除で、復元も監査記録も無い。
- **`search` は gitignore された project skill を見つけない**（`ignore::WalkBuilder` の内部挙動）。
  `read_file` / `ls` は `reject_gitignored_read_path` の skill root 例外で通る。プロンプトが
  `read_file` を名指ししているので実害は無いが、非対称ではある。
- **`pre-tool-use` の payload に反復回数が無い**。`execute_tool_call` は `iterations` を知らない。
- **hooks は経路 B に掛からない**。経路 B が `with_tools()` を本番で呼ばないことと対。
  有効化した瞬間に hook を素通りする MCP 実行経路になる。
- **`match.tools` はツール名しか見ない**。dsh はコマンドを全部 `execute` に通すので、
  `["execute"]` は事実上「毎コマンド」。コマンド内容でのフィルタが無く、hook の 5 秒予算を
  食い続ける。1 ターンの hook 総時間にも上限が無い。
- **hook は同期のみ**。観測イベント（`post-tool-use` / `response-complete`）を待たずに
  投げる口が無いので、タイムアウトが一律 5 秒であることの圧力が抜けない。
- **`.agents/skills/` を読まない**。Agent Skills 実装ガイドが相互運用の慣習として推奨している。
- **`skill_manage` の `description` 上限は 300 字**（仕様は 1024 字）。プロンプトコストを
  理由に意図的に狭めている。

### 命名（直さない）

- **プロバイダ名入りの命名**が残っている。crate `dsh-openai`、module `chatgpt`、
  `openai-execute-tool.json`。実体は OpenAI 互換 API 全般。改名は互換を壊す。
- **builtin 名の表記ゆれ**。`chat_prompt` / `chat_model` / `chat_reset`（snake）と
  `ai-commit` / `ai-watch` / `safe-run`（kebab）。

## 永続タスク (`agent`)

`dsh/src/agent.rs` がSQLiteとCLIを所有し、`AgentTaskStore` と `AgentCommandPolicy::agent_runtime` を通してループAへ渡す。`dsh-types/src/agent.rs` が状態型、`dsh-builtin/src/agent/` が記録・検証・ジョブ・任意のSRTアダプターを所有する。ループは追加しない。詳細と利用例は [../../../../agent.md](../../../../agent.md)。

結果のない変更操作を再送しない。チェックポイントに残るtool callは永続イベントの結果で補い、結果不明ならユーザーの実状態確認を要求する。予算は再開でリセットしない。モデルの最終回答だけで完了にしない。認証情報の保存、権限の外部コンテンツからの拡大、隔離失敗時の通常実行へのフォールバックは禁止。

再発防止の焦点: 対話用セッションTTLをタスクへ適用しない。互換APIのusage欠落をゼロ使用扱いしない。ジョブ結果のJSONを文字列途中で切らない。SRTがPATHで別のbashを選んでもシステム外への読み取り権限を自動で追加しない。操作結果の不明判定は表示文字列ではなく型で渡す。`Cancelled` は通常保存で解除せず、明示的な `agent resume` だけが解除する。
