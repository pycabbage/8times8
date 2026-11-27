# 8 times 8

Rust リライトの初期 headless 版です。コアは 1A の反転、1A〜1H の整数座標と範囲、レベルごとの厳密な残高・支払いを実装しています。CLI はヘルプとバージョン表示のみで、プレイはできません。盤面全体の進行、報酬・境界開放、機械探索、UI・音声、保存は未実装です。今後の方針は[技術選定](docs/design/technology-selection.md)を参照してください。

```sh
cargo run --locked -- --help
cargo run --locked -- --version
```

## 開発時のチェック

Rustup で固定の Rust ツールチェーン、rustfmt、Clippy を使います。

```sh
cargo fmt --check
cargo clippy --locked --release --all-targets -- -D warnings
cargo test --locked --release
```

## リリース

`v*` タグの push 時だけ、GitHub Actions で上記のチェックを実行し、GoReleaser で headless CLI と SHA-256 チェックサムを GitHub Releases に公開します。v0.1.1 以降の配布対象は Linux x86-64 (GNU) の `tar.gz` と Windows x86-64 (GNU) の ZIP です。どちらもヘルプとバージョン表示のみで、GUI はなくプレイできません。
