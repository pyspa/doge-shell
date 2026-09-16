# 9. 未解決の設計判断

[README.md](README.md) の §9。AI 機能の設計方針全体の索引はそちらを見る。

いずれも調査済みで根拠がある。着手する前にここを更新すること。

## 経路 A / B の非対称（一部解決）

**解決済み — 経路 B は REPL を止めない**。`ChatClient` は同期 API なので、`run_tool_loop`
（`dsh/src/ai_features/service.rs`）がそれを直接 await すると task 自体が進まなくなり、
REPL はキーハンドラを `tokio::select!` と同じ task から回すため、`Alt+d`・コマンドパレットの
AI アクション・`ai-watch` 要約・`Alt+s` の実行中は端末入力が一切読まれなかった。
`cancel_requests()` は実装済みでも、唯一の呼び出し元 `handle_interrupt` がそのキー入力経由
でしか到達しないため、**実装されていて到達不能**という状態だった（`safety.md` が別の文脈で
禁じている「登録されていることと効いていることの区別がつかない」と同型）。

直したのは 2 段:
1. `send_chat_cancellable` の呼び出しを `spawn_blocking` に移した。`cancellation_generation`
   は `Arc<AtomicU64>` になった（クロージャが `'static + Send` である必要があるため）。
   これが無いと下の `select!` はそもそも動かない。
2. `dsh/src/ai_features/await_ui.rs` の `await_with_progress` が待ち方を持つ。経過秒の 1 行
   再描画と、Esc / Ctrl-C での `cancel_requests()`。呼び出し元は spawn しない — コマンド
   パレットの `Action` は `#[async_trait(?Send)]` で `&mut Shell` を取るため spawn できない。
   **呼び出し規約**: REPL の `EventStream` が polled されていない区間でのみ呼ぶこと。型では
   表現できないので doc コメントに書いてある。端末が無いとき（`isatty` false / テスト）は
   UI ごと畳んで素の `.await` になる（`EventStream::new()` は tty 無しで panic する）。

残る制約:

- **経路 B はストリーミング非対応のまま**。`send_chat_streaming` は `ChatClient` にあるが
  `LiveAiService` は使っていない。配線するには `-> Result<String>` の呼び出し元 15 箇所以上を
  変えることになる。
- **待っている間にコマンドを打つことはできない**。`await_with_progress` は待ちを中断可能に
  するだけで、キー入力は Esc / Ctrl-C 以外読み捨てる（そのプロンプトに属するキーに答えて
  しまわないため）。
- **`Alt+s` はまだポーリング**（`repl_ai.rs` の `force_ai_suggestion`）。`AiSuggestionBackend`
  の `Notify` は**要求キューのドアベルであって結果通知ではない**ので、待てる signal が無い。
  中断可能・進捗表示付きにはなり、タイムアウト時に無言で諦めるのもやめた。
- **経路 B は `with_tools()` を本番で一度も呼ばない**（`AiRequestOptions::with_tools`
  `dsh/src/ai_features/service.rs:50`）。コマンドパレットの各アクション・ゴーストテキスト等は
  すべて `without_tools()` を使うため、`LiveAiService::run_tool_loop` の MCP 実行ブロック・
  `authorize_mcp_tool`・`ReplConfirmationHandler` はコンパイルはされるが到達しない。
  「使う」方向に配線するか、使わないと決めて周辺コードを削るかは未決定。
- **`ask_ai_async` は temperature 0.7 固定**。`blocks fix`（「修正コマンドを 1 行だけ」）にも
  同じ値が使われる。
- **シェル側 client は起動時に固定**。`AI_CHAT_API_KEY` / `AI_CHAT_BASE_URL` /
  `AI_CHAT_TIMEOUT_SECS` を後から変えても `!` チャットにしか届かない。
  起動時にキーが無いと `integration_state.ai_service` は `None` のままで、後から設定しても
  コマンドパレットと `Alt+d` は「未設定」と言い続ける。
  `AI_MESSAGE_LANG`（`refresh_derived_state` → `response_language` slot）と
  `AI_CHAT_MODEL`/`OPENAI_MODEL`（同様に `chat_model` slot、
  `Environment::reload_chat_model`）はこの制約の**外**: どちらも `IntegrationState` の
  `Arc<RwLock<Option<String>>>` を経由し、`LiveAiService::run_tool_loop` が毎リクエスト
  読んで `with_model`/`with_response_language` を被せるので、client の再構築なしに
  全経路（`!` チャット・コマンドパレットの AI アクション・ゴーストテキスト・`ai-watch`・
  `blocks explain|fix`・`output-gen`）へ届く。read-only cache（`ai_features::cache`）は
  モデルを scope に含めず、`reload_chat_model` が変更のたびに明示的に全消しする形にした
  （`answer_scope` が `std::env::var` 直読みで export しないシェル変数を見られない問題を、
  scope 化ではなく invalidate 側で解決）。ゴーストテキストの `AiSuggestionBackend` は
  `LiveAiService` を通らないので同じ slot を自分でも保持し、`ChatRequestOptions::with_model`
  を直接呼ぶ。**残る制約**: ゴーストテキストの 8 秒 TTL キャッシュ
  （`AiBackendState.cached`/`context_cached`）はモデル変更時に明示的なクリアをしないので、
  切り替え直後の最大 8 秒だけ旧モデルの候補が出うる（体感以下として許容）。

## プロバイダ API（調査済み・未着手）

- **既定モデルでは temperature が効かない**。`client.rs` の
  `OPENAI_REASONING_MODEL_PREFIXES`（`gpt-5` / `o1` / `o3` / `o4`）に当たると 1.0 を強制する。
  既定は `gpt-5-mini` なので、ゴーストテキストの 0.0 も JSON 生成の 0.1 も**すべて 1.0**。
  `reasoning_effort` を送る口（`AI_CHAT_REASONING_EFFORT`）は入ったが、temperature の代わりには
  ならない（両方ともプロバイダ側の解釈次第で、決定性を保証しない）。同じ判定関数
  `is_openai_reasoning_model` を `resolve_reasoning_effort` も読むが、2 つは別の制約を表す
  別関数（[README.md](README.md) §1 参照）。
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
  同じ弱点を持つ。`chat_model` を実行中に切り替えられるようになった分、この弱点は**広がった**:
  1 つのセッション寿命 client が実際に複数モデルを跨ぐ運用がこれで正当な使い方になったため、
  ある `tools` 付きリクエストで学んだ `reasoning_effort: none` 強制やフィールド drop が、
  その後 `chat_model` で切り替えた別モデルにも誤って適用され続けうる。今回は対処しない
  （`RecoveryState` をモデルごとに分けるのは §1 の「2 つの仕組みで済ませる」を破る）。
  `unsupported` Vec 自体は元々モデル非依存の設計（他の droppable フィールドは
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

## 対話ジョブ（実装済み・残る制約）

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
- **`tool_search` は対話に開いていない**。対話は `mcp.tool_definitions()` を全部プロンプトに
  載せるので、既に手元にあるものを探す 2 つ目の道になるだけ。

## MCP（調査済み・未着手）

- **永続接続が無い**。`list_tools` も `call_tool` も毎回接続して切る。stdio サーバは
  ツール呼び出しごとにプロセスを起動する。
- **`call_tool` のタイムアウトが 30 秒固定**、`list_tools` は**タイムアウト無し**。
- **ツールキャッシュに TTL が無い**。`ToolCacheEntry.timestamp` は書かれるだけで読まれない。
- **`unique_name` の連番が登録順に依存**する。リロードで同じツールの function 名が
  変わりうる（`mcp:<function_name>:<args>` のセッション承認がそこで無効化される）。

## Skill / hooks（調査済み・未着手）

- **skill のライフサイクル自動遷移は実装済み**（[skill.md](skill.md) 参照）。`archived_ms` / `pinned` を
  `SkillUsage` に追加し `STATE_VERSION` は上げていない。archive は「ファイルを動かす」ではなく
  state フラグなので `install-runtime-skills.sh --check-installed` と `doctor skills` の
  drift 検査に影響しない。CLI 限定（`skill archive`/`unarchive`/`pin`/`unpin`）で
  `skill_manage` に action は増やしていない。**残る既知の制約**: 旧バージョンの dogesh が
  `usage::flush()` を一度でも実行すると（serde の未知フィールド読み捨てにより）
  `archived_ms`/`pinned` が消える — 破壊的ではない（archive が解除されプロンプトに戻るだけ）が
  再発しうる。`flush_to` の「存在しないディレクトリのレコードを消す」規則により、
  archive → ディレクトリ削除 → 再インストールでも archive 状態は失われる。
  `skill_manage delete` / `skill remove` は依然即削除で、復元も監査記録も無い。
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

## 命名（直さない）

- **プロバイダ名入りの命名**が残っている。crate `dsh-openai`、module `chatgpt`、
  `openai-execute-tool.json`。実体は OpenAI 互換 API 全般。改名は互換を壊す。
- **builtin 名の表記ゆれ**。`chat_prompt` / `chat_model` / `chat_reset`（snake）と
  `ai-commit` / `ai-watch` / `safe-run`（kebab）。
