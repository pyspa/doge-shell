# Invariants: 安全判定

[../invariants.md](../invariants.md) の索引から。短いが破りやすいルール。
AI 機能側の安全ゲート方針は [../ai/safety.md](../ai/safety.md) にもある（そちらが §4 の正典、
これは事故った箇所だけを載せた短いルール集）。

- `SafetyGuard::check_jobs` は `Job.cmd`（**行全体。パイプラインもまとめて 1 本の文字列**）を見る。先頭トークンだけを分類すると `true | rm -rf ~` は `true`、`sudo rm -rf ~` は `sudo` になり、どちらもルールが無いので**全チェックを素通りする**。オペレータで区切り、ラッパーを覗いてから分類すること（`dsh_types::safety_policy::{split_command_segments, command_candidates}`）。
- 行の分割は**生文字列**に対してやる。`shell_words::split` は空白でしか切らないので `echo hi; rm -rf ~` は `["echo", "hi;", "rm", ...]` になり、トークン単位の分割では `;` が見えない。
- ラッパーのオプションは値を取る（`timeout 5 ...`、`nice -n 10 ...`、`chroot /new ...`）。「最初の非オプション引数が中身のコマンド」は**その値を拾う**。`command_candidates` は残りの非オプショントークンを全部候補にして fail-safe に倒している。
- **判定した行と実行する行を一致させる**。`sh -c` は行全体を実行するのに、dogesh の文法は grouping・制御構文・heredoc を持たない。`get_jobs` は未消費の末尾を**警告するだけ**なので、安全判定側は `unconsumed_tail` と `compound_statement_keyword` で fail closed にする。`{ rm -rf ~; }` は `{` という名前のコマンドとして完全にパースされてしまう。
- コマンド置換（`` ` ``、`$(...)`、`<(...)`、`(...)`）は**判定より前に拒否する**。`shell::parse::parse_command` が評価するので、「安全か」を尋ねること自体が実行になる。
- 文字列をコードとして渡す経路は flag だけではない。stdin から読むシェル（`printf ... | sh`）、入力リダイレクト（`bash < script.sh`）、`eval` は flag を持たない（`execute.rs` の `hidden_code_source`）。`shell_words::split` は `<` を独立トークンにするので「全引数が `-` 始まり」では捕まらない。
- MCP ツール名はモデルから見ると `mcp__<label>__<tool>`。素の名前で `matches!` すると**分岐が一度も成立しない**（`is_mcp_command_execution_tool` が実際そうだった）。判定は `McpManager::tool_name_for` が引いた実ツール名で行い、allowlist entry と質問文は function name のまま使う。
- `SafetyResult` は `Allowed | Confirm` の 2 値。拒否は `AgentCommandVerdict::Denied`。常に `None` を返す checker を登録しない（登録の有無が挙動と一致しなくなる）。
- `mcp disconnect` は bindings に効く（`tool_definitions` / `system_prompt_fragment` / `execute_tool` が disabled サーバを外す）。`session_meta` は明示 `mcp connect` でしか埋まらないので、そこをゲートに使うと起動時ロードだけのサーバが全滅する。
- `turn::truncate_middle` の予算は**バイト数**（引数名は `max_chars`）。日本語では実効が約 1/3。AI へ渡す文字列を自前で `&s[..n]` しない（`safe_run` がそれで panic していた）。
- `Path::join` は空パスを与えると区切りを足す。`PathBuf::new()` から接尾辞を積むと `notes.txt/` になり、`fs::write` が ENOENT で落ちる（`resolve_with_existing_ancestor`、`edit` が新規ファイルを作れなかった）。
