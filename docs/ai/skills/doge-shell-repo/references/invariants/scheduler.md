# Invariants: スケジューラ

[../invariants.md](../invariants.md) の索引から。短いが破りやすいルール。

- `Shell` は `!Send`（`Rc<RefCell<LispEngine>>`）。spawn したタスクから `eval_str` は呼べない。
- `handle_background_tick` は `tokio::select!` の腕の中で await される。ここで重い処理をするとキー入力が固まる。
- 履歴同期の SQLite 読み込み・正規化と command timing の JSON 書き込みを REPL イベントループへ戻さない。`repl/background_io.rs` で予約し、完了後は世代を確認してメモリ上の snapshot だけを適用する。
- command history の reload は SQLite 保存未確認のローカル差分を snapshot へマージする。command timing は一時ファイルから atomic に公開し、`timing --clear` 後の古い snapshot を reset 世代で拒否する。
- 非ブロッキング性は時間閾値ではなく `repl::background_io::tests` の停止ワーカー・in-flight・完了 channel で検証する。世代競合は `command_timing::tests` と `history::*::background_reload_tests` が入口。
- REPL 終了時は raw mode と DECSTBM を解除してから background writer の完了を待つ。ファイル I/O 待機中に端末状態を保持しない。
- pause から resume するときは `next_run` を貼り直す。しないと溜まった分が一斉に発火する。
