# 8. AI chat hooks

[README.md](README.md) の §8。AI 機能の設計方針全体の索引はそちらを見る。

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

## Regression チェックリスト（`task-map.md` から）

AI chat hooks / ai-hooks.json / pre-tool-use / 外部コマンドを触るときに確認する短い要約。
詳細は上の各項目。

`HookDecision` に `Allow` を足さない。gate イベントは fail-closed、観測イベントは fail-open。
payload は stdin、ただし pipe ではなく一時ファイル（pipe だと孫が read 端を握ったときシェルが
ハングする）。`command[0]` はロード時に絶対パス化し、相対パスは拒否。権限検査は
world-writable のみ。ターン末処理は全離脱経路を通す。`session-start` は checkpoint 再開では
鳴らさない。**`match` の照合は redact 前の生の引数で行う**（`-p` マスクで `/etc/**` の hook が
発火しなくなる = 許容側に倒れる）。引数が読めないときは一致させる。イベントを足したら
`HookEvent::ALL` に入れる。**ターン予算の超過は観測イベントだけスキップし、gate は残り時間
まで縮めて必ず実行する**（縮んだ timeout 超過は既存規則で deny）。`programs` は `execute` の
allowlist マッチャを再利用するが**極性が逆**なので、片方を厳しくするともう片方のゲートが
弱まる。

検証: `cargo test -p dsh-builtin --lib chatgpt::hooks`; `cargo test -p dsh-builtin --lib chatgpt::tool::tests`
