# 4. 安全ゲートは `SafetyGuard` ただ 1 つ

[README.md](README.md) の §4。AI 機能の設計方針全体の索引はそちらを見る。

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
- **read marker は語単位、mutating marker は部分一致**（`is_read_only_mcp_tool`）。この非対称が
  安全性そのもの: mutating は過剰包含（余計な質問が出るだけ）、read は過小包含（同上）。
  read を部分一致にしていたせいで `ls` が `emails` / `labels` / `channels` / `urls` に当たり、
  `send_emails` や `add_labels` が Normal で無確認実行されていた。語の切り出しは
  `SafetyGuard::words`（区切り文字 + camelCase、`getHTTPStatus` → `get`/`http`/`status`）。
- **サーバの側作用宣言は「厳しくする方向」にだけ信じる**（`McpToolCall::declared_read_only`、
  `McpManager::tool_facts_for` が引く）。`readOnlyHint: false` と `destructiveHint: true` の
  どちらも「副作用がある」の意で、仕様上 `readOnlyHint` の既定が false なので後者だけを送る
  サーバがある。`ToolAnnotations` は `None` を serialize しないので、**見えた値はサーバが
  送ることを選んだ値**であって既定値ではない。**逆は成り立たない** — `readOnlyHint: true` で
  確認を飛ばさない。サーバは確認が守ろうとしている相手そのもので、自分についての自己申告で
  ゲートを開けられてはいけない。両経路（`AgentCommandPolicy` と
  `LiveAiService::authorize_mcp_tool`）が同じマネージャに訊き、名前と宣言は
  `tool_facts_for` の**1 回のロック**で揃える（別々に引くと `mcp connect` やツール一覧更新を
  跨いで、あるサーバのツール名に別のサーバの宣言が付きうる）。
- **`McpToolCall` は struct で渡す**。`function_name` と `tool_name` は隣り合う `&str` で
  取り違えてもコンパイルが通り、判定だけが静かに変わる（`mcp__ops__bash` は `"bash"` に
  一度も一致しないので、シェルツールが実行するコマンドとして判定されなくなる）。
  一度実際に起きた退行なので型で塞ぐ。
- ツール一覧からバインディングを作るのは **`mcp::bind_tool` 1 箇所**。以前は起動時・リフレッシュ時・
  `mcp add` の 3 箇所に手書きの複製があり、`ToolBinding` にフィールドを足すと 2 箇所にだけ届いて
  残り 1 箇所が静かに欠ける、という形のバグを許していた（テストは手書きのバインディングを使うので
  全部通る）。
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
  `dsh-builtin/src/chatgpt/tool/safety_gates.rs` の `confirm_agent_action` を通す。
  **質問文に "Proceed?" を書かない**。`repl/confirmation.rs` が
  `Proceed? [y/N/a(Always)]:` を付けるので、書くと 2〜3 回出る。
- **書き込みは差分を見せてから訊く**（`confirm_agent_action_with_preview`）。ファイル名だけの
  質問は人が答えられないので、合理的な反応は「読むのをやめて `a` を押す」になり、そこでゲートが
  ゲートでなくなる。差分は `dsh-builtin/src/diff.rs` の `preview`（`skill diff` と同じ
  `unified_lines` を共有。2 本目の diff 実装を作らない）で、変更のない行は畳み、長い行は切り、
  `MAX_PREVIEW_LINES` で打ち切り、`redact_sensitive_text` を通す。質問文には `+3 -1` の要約が入る。
  **プレビューを出すのは「実際に人に訊く枝」だけ** — タスクの `stop_reason` に入れると保存レコードと
  `cron logs` の incident 本文が膨らみ、セッションの "always" 済みなら訊かないのだから出す意味がない。
  対象は `edit` / `str_replace` / `skill_manage` の書き込み。`!` チャットの間は raw mode が
  off（`shell/eval.rs`）なので素の `\n` でよい。`confirm_action` 側は raw mode で描くので
  `crlf()` で改行を正規化する。
- `loose` は「コマンド・MCP・機微読み取りを素通りさせる」であって「全部素通り」ではない。
  **ファイル書き込み（`edit` / `str_replace` / `skill_manage`）と skill script はレベルに関係なく必ず確認する。**
  skill script の判定は user scope だけでなく **project scope（`<project>/.dogesh/skills`）も含む**
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
