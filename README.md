# 8 times 8

Rust リライトの初期 headless コアです。1A の反転、1A〜1H の整数座標と範囲、レベルごとの厳密な残高・支払いを実装しています。

```sh
cargo run --locked -- demo
cargo run --locked -- --help
cargo run --locked -- --version
cargo test --locked --release
```

デモは単独の 1A ブロックを 3 枚、次に 61 枚反転して完成させます。盤面全体の進行、報酬・境界開放、機械探索、UI・音声、保存、配布は未実装です。今後の方針は[技術選定](docs/design/technology-selection.md)を参照してください。
