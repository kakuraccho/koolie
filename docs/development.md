# 開発

## ビルドとテスト

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
```

コミットする前に、test、clippy、fmt の 3 つをすべて通します。

## テストの層

テストは 3 つの層に分けています。

| 層 | 場所 | 何を確かめるか |
|---|---|---|
| 単体テスト | 各モジュールの `#[cfg(test)]` | 下の表を参照 |
| 結合テスト | `tests/fake_herdr.rs` | 偽の herdr サーバーを相手に、接続 → 購読 → スナップショット、`events_lost` からの復旧、ペインの増減に合わせた購読し直し、送信 |
| 実機（`#[ignore]`） | 一部のテスト | 動いている herdr が必要なもの。`cargo test -- --ignored` で実行する |

単体テストで確かめていること:

| 対象 | 確かめていること |
|---|---|
| model / backend | JSON を型に変換すること（herdr 0.9.5 の実物のスナップショットを固定データにしている）、並び順 |
| transcript | Claude Code / Codex の会話記録の解析 |
| usage | 使用量の集計 |
| discover | ssh config の読み取り、probe の出力の解釈 |
| session | 接続の状態の移り変わり |
| app | キー操作と画面の状態 |

- SSH のテストは、`KOOLIE_SSH` に偽の `ssh` スクリプトを指定して行います。本物のネットワークには出ません。
- テストからネットワークへは出ません。動いている herdr が必要なテストには `#[ignore]` を付けます。

## 実機で確かめる

単体テストでは見つからない問題が実際にありました。入力欄にフォーカスがなく、打った文字がコマンドとして解釈されてしまう、表示が崩れる、といったものです。画面に関わる変更をしたら、実際に動かして確かめます。

### TUI を tmux で動かして画面を読む

`scripts/tui-drive.sh` で、TUI を見えない tmux セッションで動かし、キーを送って画面を読めます。

```sh
cargo build
scripts/tui-drive.sh start "./target/debug/koolie --demo"
scripts/tui-drive.sh keys Down Enter     # tmux send-keys と同じ書き方
scripts/tui-drive.sh lit "こんにちは"     # 文字をそのまま打つ
scripts/tui-drive.sh shot                # 今の画面を表示
scripts/tui-drive.sh stop
```

画面の大きさは `COLS` と `ROWS`、キーを送った後の待ち時間は `WAIT` で変えられます。

### 小さい端末で落ちないか

```sh
cargo build && scripts/tiny-sizes.sh
```

2×2 から 60×12 までの大きさで、すべての画面を順に開きます。すべての行が `EXITED:0` になり、panic の出力がないことを確かめます。

### 本物の herdr で確かめる

自分専用のワークスペースを作り、終わったら閉じます。ユーザーが使っているペインには、絶対に入力しません。

```sh
herdr workspace create --no-focus --label koolie-test
# このワークスペースでエージェント（または偽のエージェント）を動かす
herdr workspace close <id>
```

- 偽のエージェントの作り方は 2 つあります。
  - `herdr pane report-agent` で、状態を手で報告する。
  - `claude` という名前のスクリプトを動かす。
- `agent.prompt` で送るには、ペインの前面のプロセスが、その名前のエージェントである必要があります。そうでないと `agent_not_ready` になります。
- `herdr server stop` は実行しません。ユーザーのセッションが全部止まります。

## レビュー

大きめの変更をしたら、読むだけのレビューを別のエージェントに頼み、指摘を直します。そのときの決まりは次の 2 つです。

- **修正には、直す前に落ちるテストを付けます。** 修正を外してテストが落ちることを確かめます。これまでに「直さなくても通るテスト」が 2 件見つかっています。
- **修正した差分も、もう一度レビューします。** 1 回目の修正で、新しいバグが 3 件入ったことがあります。

## コードの決まり

- 知らない JSON のフィールドは無視します。herdr はフィールドを増やしていきます。
- テスト以外のコードでは、サーバーやファイルから来たデータに `unwrap()` を使いません。
- リモートで実行する sh スクリプトには、値を引数で渡します。スクリプトの文字列には埋め込みません。
- 画面に出す文は日本語にします。

## 価格表を更新する

`src/usage/pricing.json` は、LiteLLM の `model_prices_and_context_window.json` から、必要なモデルだけを抜き出したものです（MIT。`src/usage/pricing.LICENSE`）。

モデル名は前方一致で、いちばん長く一致したものの価格を使います。表にないモデル（例えば現時点の `claude-fable-5-2`）は、近い名前のモデルの価格で計算されます。正確な概算が必要なら、表に足してください。
