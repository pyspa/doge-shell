# Invariants: プラットフォーム

[../invariants.md](../invariants.md) の索引から。短いが破りやすいルール。
詳細な作業手順は [platform-support.md](../platform-support.md) にもある。

- `nix` の Linux 拡張を使わない。`pipe2` は nix が macOS に出しておらず、これで `doge-shell` crate 全体がコンパイル不能だった（`a846e3c`）。cloexec な pipe は `std::io::pipe` が両 OS でくれる。
- `rustflags` は `[target.'cfg(target_os = "linux")']` の下に置く。`[build]` に書くと macOS の clang が `-fuse-ld=mold` を `invalid linker name` で拒否し、**リンクが全部落ちる**（`f866418`）。
- `/bin/true` と `/bin/false` は macOS に無い（`/usr/bin` にしかない）。テストからの絶対パス起動は `dsh/tests/common/mod.rs` の `true_path()` / `false_path()` / `first_existing()` を通す。`/etc/hostname` も macOS に無いので `/etc/hosts` を使う。`/tmp` は macOS で `/private/tmp` に解決されるので `canonicalize` して比べる（`ae2f192`）。
- macOS の `/etc/passwd` は**実在するのに実質空**。単一ユーザーモード用で、対話ユーザーは Open Directory にいる。ファイルの有無で分岐すると「読めたのに `root` しか出ない」になる（`f45fcc2`）。`/etc/group` は逆に macOS でも埋まっているが、Open Directory が足すグループは持たない。
- シグナル番号は 1-15 しか共通でない。`SIGUSR1` は Linux 10 / macOS 30、`SIGCHLD` は 17 / 20。番号表を共有せず per-OS に持ち、`libc` と突き合わせるテストを付ける（`59855e1`、`generators/signal.rs`）。
- `dirs::config_dir()` と `xdg::BaseDirectories` は Linux で同じ、macOS で**別のディレクトリ**（前者は `~/Library/Application Support`）。混ぜると installer が書いた場所を loader が読まない。実際に runtime skill が macOS でエージェントから見えなかった。config パスは `dsh-builtin/src/config_paths.rs`（`dsh` crate 内は `environment::get_config_file`）を通す。`scripts/check-portability.py` が直接呼び出しを禁止している。
- 片肺の `#[cfg]` は**何も落とさない**。もう一方の OS でその項目が存在しなくなるだけで、コンパイルもテストも通る。`scripts/check-portability.py` がファイル単位で見るのが唯一の自動防波堤で、関数単位は CI の macos ジョブが担う。
