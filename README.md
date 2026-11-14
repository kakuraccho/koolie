# koolie

herdr で動いているコーディングエージェントを、ターミナルからまとめて見て操作するための TUI です。iOS アプリ [Heeler](https://github.com/ZingerLittleBee/Heeler) の体験をターミナルに持ってきたもので、このマシンのエージェントも、SSH や tailnet でつながる別マシンのエージェントも 1 つの一覧に並べます。

- 入力待ち (blocked) と完了 (done) のエージェントを一覧の先頭に出します。
- エージェントの画面を読む、メッセージを送る、入力待ちの質問にキー 1 つで答える、端末に入る、が一覧から直接できます。
- Claude Code と Codex は、会話の記録からチャット形式で読めます。
- blocked / done になったときにデスクトップ通知を出します。画面なしで常駐させることもできます。
- Claude Code と Codex のトークン使用量と、API 定価での概算コストを出します（請求額ではありません）。
- 対応しているサーバー: Herdr TUI（socket API）と herdr-core。

## インストール

```sh
cargo install --path .
```

Rust 1.85 以降（edition 2024）が必要です。

## 使い方

```sh
koolie            # TUI を起動（このマシンの herdr は自動で見つけます）
koolie --demo     # herdr なしで画面を試す
```

主なキー（`?` で全部出ます）:

| 画面 | キー | 動作 |
|---|---|---|
| 一覧 | `j` `k` / `Enter` / `c` | 移動 / 詳細を開く / チャットで開く |
| 一覧 | `a` | エージェントの端末に入る（Ctrl+B → q で戻る） |
| 一覧 | `/` `n` `b` `d` `w` `i` `h` | 検索 / 要対応 / 入力待ち / 完了 / 作業中 / 待機 / ホストで絞り込み |
| 一覧 | `H` / `u` | ホスト管理 / 使用量 |
| 詳細 | 文字入力 → `Enter` | メッセージを送る（`Alt+Enter` で改行） |
| 詳細 | `Tab` | 画面 ⇄ チャット |
| 詳細（入力待ち） | `1`-`9` `y` `n` `↑` `↓` `Enter` | そのキーをエージェントに直接送る |
| 詳細 | `Esc` | 操作モードへ（もう一度で一覧へ） |

入力待ちのエージェントに書いたメッセージは、確定せずに入力欄へ入れるだけにしています（画面の質問に先に答える必要があるため）。

### 別のマシンを追加する

`H` → `a` で候補（`~/.ssh/config`、tailnet のピア、herdr の保存済みマシン）から選ぶか、SSH の接続先を手入力します。koolie は相手のマシンで読み取り専用の調査を 1 回だけ実行し、見つかった herdr のセッションを登録します。相手のマシンには何もインストールしません。

- SSH はシステムの `ssh` をそのまま使うので、`~/.ssh/config`、ssh-agent、ProxyJump、Tailscale SSH が効きます。
- パスワードは聞きません（`BatchMode=yes`）。鍵認証で入れるようにしておいてください。認証が必要なホストは「要認証」と表示されます。
- 1 つのホストに経路を複数書けます（例: LAN → tailnet の順に試す）。

CLI でも追加できます:

```sh
koolie hosts discover          # 追加できそうなマシンの一覧
koolie probe studio.tail1234.ts.net
koolie hosts add studio.tail1234.ts.net --name studio
koolie hosts list
```

### 常駐と通知

```sh
koolie watch                   # 画面なしで接続を保ち、blocked / done を通知
koolie status --format tmux    # tmux のステータス行用（watch が動いていればその結果を使う）
```

tmux の例: `set -g status-right '#(koolie status --format tmux)'`

### 使用量

```sh
koolie usage                   # 直近 30 日
koolie usage --json --days 7
```

リモートのマシンの使用量は、そのマシンに koolie が入っていれば TUI の使用量画面に出ます。

## 設定

`~/.config/koolie/config.toml`（`KOOLIE_CONFIG` で変更可）:

```toml
[[host]]
name = "studio"
kind = "herdr-tui"            # または "herdr-core"
routes = ["studio.lan", "studio.tail1234.ts.net"]

[notify]
on = ["blocked", "done"]
sinks = ["desktop", "bell"]   # "command" も可
command = "curl -d \"$KOOLIE_AGENT: $KOOLIE_STATUS\" ntfy.sh/my-topic"

[[snippet]]
name = "続けて"
text = "続けてください"
```

## チャット表示の前提

チャット表示には、エージェントのセッション情報を herdr が知っている必要があります。そのマシンで次を実行し、エージェントを起動し直してください。

```sh
herdr integration install claude
herdr integration install codex
```

## 開発

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

`tests/fake_herdr.rs` は本物と同じ形式で話す偽の herdr サーバーを立てて、接続・購読・送信を確かめます。

使用量の価格表 `src/usage/pricing.json` は LiteLLM のデータ（MIT）に基づいています（`src/usage/pricing.LICENSE`）。
