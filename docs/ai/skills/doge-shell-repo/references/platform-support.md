# Platform Support

対応 OS の唯一の normative な記述。実際に事故った事実だけを載せる。

## 対応 OS

- **Linux** (x86_64 / aarch64, glibc / musl) と **macOS** (Apple Silicon / Intel)。どちらも同格で、片方でしか動かない変更を入れない。
- Windows は非対応。`std::os::unix` の `RawFd` / `PermissionsExt` / `CommandExt` に 40 箇所以上依存している。`#[cfg(unix)]` / `#[cfg(not(unix))]` の対がいくつかあるが、これは Windows を意識した防御で macOS 差分ではない。
- `dsh/src/lib.rs` と `dsh-builtin/src/lib.rs` の `compile_error!` が対応 OS を 2 つに固定する。だから木全体の `#[cfg(not(target_os = "macos"))]` は「Linux」と読んでよい。

## 層ごとの要求

- **コア** (パーサ / REPL / ジョブ制御 / PTY / 履歴 / 補完エンジン): 両 OS で `cargo clippy --workspace --all-targets -- -D warnings` と `cargo test --workspace` が通り、挙動が一致すること。
- **host-fact provider** (`dsh/src/completion/generators/`, `dsh/src/completion/dynamic.rs` の user / group / process / interface / signal / mount / sysctl): 片方の OS にしかないソースを読むなら、**もう一方の OS 用のソースを必ず用意する**。「ファイルが無いので静かに 0 件」は不可。macOS の `/etc/passwd` は実在するのに実質空、というのが最悪の形で、これを踏んだ (`f45fcc2`)。
- **OS 固有 CLI の補完定義** (`completions/pacman.json`, `systemctl.json`, `brew.json` …): パリティは不要。Linux 専用の定義が多数あって macOS 専用はほぼ `brew.json` だけ、という非対称は仕様通り。要求は「そのコマンドが無い OS でエラーもハングも起こさず静かに 0 件になる」ことだけ。

## 分岐の書き方

- `#[cfg(not(target_os = "macos"))]` と `#[cfg(target_os = "macos")]` を必ず**対で**書く。片方だけ書くと、もう一方の OS では**その項目が存在しないだけ**で、コンパイルも通りテストも落ちない。`scripts/check-portability.py` の片肺 cfg 検査がこれを見る。
- 共通ロジックは cfg の外の純粋関数に置く。手本は `dsh/src/completion/generators/user.rs` の `is_offered`: OS ごとに違うのは「どこから読むか」と定数だけで、判定は 1 つ。
- OS ごとに違う定数表には `libc` に突き合わせるテストを付ける。手本は `dsh/src/completion/generators/signal.rs` の `the_table_uses_this_platforms_signal_numbers`。ただしこのテストは**走ったホストの側だけ**を検証するので、CI の両ジョブが揃って初めて完全になる。
- per-OS の依存は `[target.'cfg(target_os = "macos")'.dependencies]` へ。`dsh/Cargo.toml` の `sysinfo` と `nix` の `net` feature が唯一の実例。追加する前に他 crate で既に無条件依存になっていないか確認する (`sysinfo` は `dsh-builtin` では全 OS で入っているので、`dsh` 側を macOS 限定にした節約効果は限定的)。
- テストから外部コマンドを絶対パスで呼ぶときは `dsh/tests/common/mod.rs` の `true_path()` / `false_path()` / `first_existing()` を使う。`/bin/true` と `/bin/false` は macOS に無く `/usr/bin` にしかない。`/bin/sh` `/bin/echo` `/bin/ls` `/bin/cat` は両方にある。

## macOS 側の腕を Linux ホストで確認する

- `cargo check` / `cargo clippy` は**そのホストの腕しか見ない**。macOS 側の腕は Linux では型検査すらされない。「clippy が通った」は macOS で通ることを何も意味しない。
- **クロスコンパイルは使えない**。`cargo check --target aarch64-apple-darwin` は `rusqlite` (bundled SQLite の C ビルド) と `mac-notification-sys` (`cc` で Objective-C をビルド) が Apple SDK を要求して失敗する。`cargo-zigbuild` も Foundation / AppKit のヘッダを持たないので同じ。試さないこと。
- Linux でできる近似: 対象ファイルの `#[cfg(target_os = "macos")]` を `#[cfg(all())]`、`#[cfg(not(target_os = "macos"))]` を `#[cfg(any())]` に一時置換し、`dsh/Cargo.toml` の macOS 限定 dep を一時的に無条件へ移して `cargo check -p doge-shell`。終わったら必ず戻す。`59855e1` / `f45fcc2` はこの手順 (を macOS 側から見たもの) で検証された。
- **唯一の実証は CI の macos-latest ジョブか実機**。`.github/workflows/ci.yml` がそれ。

## `scripts/check-portability.py` が見るもの / 見ないもの

- 見る: allowlist に無い OS 固有パスリテラル、**ファイル単位で**片肺になった `target_os` 分岐、`else` の無い `cfg!(target_os = ..)`、`[target.'cfg(..)']` の外の `rustflags`、`compile_error!` ガードの消失。コメント行は走査しない。
- 見ないもの 1: **関数単位の対応漏れ**。判定はファイル単位なので、既に対を持つファイルに片肺の関数を足しても通る。これは意図的な妥協で、`dynamic.rs` の `collect_sysctl_keys`（Linux 側の腕だけが持つ再帰ヘルパ）や `user.rs` の macOS 限定テストヘルパのように、片側だけが正しい項目が実際に多いため。関数単位の保証は CI の macos ジョブが担う。
- 見ないもの 2: macOS 側の腕が**コンパイルできるか**。Linux ホストでは型検査すらされない。
- 新しいリテラルを足したいときは `--update` で allowlist を再生成し、その差分をレビューで見せる。エントリが消えたときも落ちる（ratchet は緩まない方向にだけ動く）。

## 意図的に Linux 専用な領域

- `dsh/src/completion/dynamic/linux.rs` — systemd / Arch / SELinux / netfilter / snapper。Linux 専用ソースを読んでよい唯一の場所。macOS では全 collector が静かに 0 件を返すのが正しい挙動。
- `dsh/src/completion/dynamic.rs` の Linux 系ローダ — kernel module、blkid、firewalld、ipset、wireguard、journal、keymap 等。
- どちらも `scripts/portability-allowlist.txt` に `パス<TAB>リテラル` の対で固定されている。

## ツールチェーンとビルド

- MSRV 1.91 (`Cargo.toml` の `rust-version`)。`str::floor_char_boundary` (dsh-openai) と `std::io::pipe` が根拠。
- mold は Linux のみ。`.cargo/config.toml` は `[target.'cfg(target_os = "linux")']` の下に書く。`[build] rustflags` に置くと macOS の clang が `-fuse-ld=mold` を `invalid linker name` で拒否して**全リンクが落ちる** (`f866418`)。`scripts/check-portability.py` がこれを見る。
- mold が PATH に必要。CI の Linux ジョブは `apt-get install -y mold` を踏む。
- `[profile.dev] strip = "none"` は macOS 用。`debug = 0` だと cargo が `strip = "debuginfo"` を既定にし、macOS では `rust-objcopy` (optional な `llvm-tools` component) を要求して毎ビルド警告が出る。
- `nix` の Linux 拡張 API を使わない。`pipe2` は nix が macOS に出しておらず、これで crate 全体がコンパイル不能だった (`a846e3c`)。`std::io::pipe` が両 OS で cloexec な pipe をくれる。
