# koolie

herdr で動いているコーディングエージェントを、ターミナルからまとめて見て操作するための TUI です。iOS アプリ [Heeler](https://github.com/ZingerLittleBee/Heeler) でできることをターミナルでもできるようにしたもので、このマシンのエージェントと、SSH や tailnet でつながる別のマシンのエージェントを 1 つの一覧に並べます。

- 入力待ち (blocked) と完了 (done) のエージェントを一覧の先頭に出します。
- 一覧から直接、次のことができます。
  - エージェントの画面を読む
  - メッセージを送る
  - 入力待ちの質問にキー 1 つで答える
  - 端末に入る
- Claude Code と Codex は、会話の記録からチャット形式で読めます。
- blocked / done になったときにデスクトップ通知を出します。画面なしで常駐させることもできます。
- Claude Code と Codex のトークン使用量と、API 定価での概算コストを出します（請求額ではありません）。
- 対応しているサーバー: Herdr TUI（socket API）と herdr-core。

まだ確認できていないこと（SSH 越しの実機接続など）は [docs/status.md](docs/status.md) にまとめています。

## インストール

```sh
cargo install --path .
```

Rust 1.89 以降が必要です（依存している notify-rust / zbus の要求）。Linux と macOS が対象です。

## 使い方

```sh
koolie            # TUI を起動（このマシンの herdr は自動で見つけます）
koolie --demo     # herdr なしで画面を試す
```

### キー操作

`?` でいつでも一覧を出せます。

**一覧**

| キー | 動作 |
|---|---|
| `j` `k` / `g` `G` | 移動 / 先頭・末尾へ |
| `Enter` / `c` | 詳細を開く / チャットで開く |
| `a` | エージェントの端末に入る（herdr のデタッチ操作、通常は `Ctrl+B` → `q` で戻る） |
| `/` | 検索 |
| `n` `b` `d` `w` `i` | 絞り込み: 要対応 / 入力待ち / 完了 / 作業中 / 待機（もう一度押すと解除） |
| `h` | ホストで絞り込み（押すたびに次のホスト） |
| `r` | すべて取り直す |
| `H` / `u` | ホスト管理 / 使用量 |
| `q` | 終了 |

**詳細**

詳細を開くと入力欄にカーソルがあります（エージェントが入力待ちのときを除く）。

| キー | 動作 |
|---|---|
| 文字入力 → `Enter` | メッセージを送る（`Alt+Enter` か `Ctrl+J` で改行） |
| `Ctrl+T` | スニペット（定型文）を入れる |
| `Esc` | 入力欄を離れて操作モードへ |

操作モードのキー:

| キー | 動作 |
|---|---|
| `i` / `Enter` | 入力欄に戻る |
| `Tab` / `c` | 画面 ⇄ チャット |
| `j` `k` / `G` | スクロール / 最新へ |
| `a` | 端末に入る |
| `x` | 中断（エージェントに `Esc` を送る） |
| `s` | スニペット |
| `Esc` / `q` | 一覧へ戻る |

エージェントが入力待ちのとき（画面表示のみ）は、`1`-`9` `y` `n` `↑` `↓` `←` `→` `Enter` をそのままエージェントに送ります。表示が更新される前の押し間違いを防ぐため、入力待ちになってから 0.7 秒間はキーを送りません。

入力待ちのエージェントに書いたメッセージは送信せず、エージェントの入力欄に入れるだけにしています。画面に出ている質問に先に答える必要があるためです。

**ホスト管理**（`H`）: `a` 追加、`r` / `Enter` 再接続、`x` 削除（`y` で確定）。

**使用量**（`u`）: `Tab` / `←` `→` でホストを切り替え、`r` で集計し直します。

### 別のマシンを追加する

`H` → `a` で、追加するマシンを選びます。

- 候補は `~/.ssh/config`、tailnet のピア、herdr に保存済みのマシンから出ます。
- SSH の接続先を手で入力することもできます。

koolie は相手のマシンで読み取りだけの調査（probe）を 1 回実行し、見つかった herdr のセッションを登録します。相手のマシンには何もインストールせず、サーバーの起動もしません。

- SSH はシステムの `ssh` をそのまま使います。そのため `~/.ssh/config`、ssh-agent、ProxyJump、Tailscale SSH がそのまま使えます。
- パスワードは聞きません（`BatchMode=yes`）。鍵認証で入れるようにしておいてください。鍵で入れないホストは「要認証」と表示されます。
- 1 つのホストに経路を複数書けます（例: LAN → tailnet の順に試す）。

CLI でも追加できます:

```sh
koolie hosts discover                        # 追加できそうなマシンの一覧
koolie probe studio.tail1234.ts.net           # そのマシンで動いている herdr を調べる
koolie hosts add studio.tail1234.ts.net --name studio
koolie hosts add mac --core                  # herdr-core を登録する
koolie hosts list
koolie hosts remove studio
```

`koolie probe local` で、このマシン自身を調べられます。

### 常駐と通知

```sh
koolie watch                   # 画面なしで接続を保ち、blocked / done を通知
koolie status                  # 「blocked 2 / done 1」のような短い要約
koolie status --format tmux    # tmux のステータス行用（json も可）
```

- `watch` は集計を `$XDG_RUNTIME_DIR/koolie/status.json` に書き続けます。`status` はこのファイルを読みます。`watch` が動いていないときは、このマシンの herdr だけを 1 回調べます。
- tmux の例: `set -g status-right '#(koolie status --format tmux)'`
- `watch` がなくても、TUI を開いている間は TUI が通知します。同じホストに `watch` もつながっているときは、TUI はデスクトップ通知と `command` を出さず（`watch` が出すため）、ベルと画面内の表示だけにします。

systemd のユーザーサービスにする例（未検証）:

```ini
# ~/.config/systemd/user/koolie-watch.service
[Unit]
Description=koolie watch

[Service]
ExecStart=%h/.cargo/bin/koolie watch
Restart=on-failure

[Install]
WantedBy=default.target
```

### 使用量

```sh
koolie usage                   # 直近 30 日（日・モデル・プロジェクト別）
koolie usage --json --days 7
```

- 会話の記録（`~/.claude/projects/`、`~/.codex/sessions/`）を読んで集計します。読んだ位置を `~/.cache/koolie/usage/` に保存し、2 回目以降は増えた分だけ読みます。
- 価格表はバイナリに同梱しています。実行中にネットワークから取りに行くことはしません。
- リモートのマシンの使用量は、そのマシンに koolie が入っていれば TUI の使用量画面に出ます。

## 設定

設定ファイルは `~/.config/koolie/config.toml` です（`--config` か `KOOLIE_CONFIG` で変えられます）。なくても動きます。このマシンの herdr は自動で登録されます。

```toml
[local]
auto = true                   # このマシンの herdr を自動で登録する（既定 true）

[[host]]
name = "studio"
kind = "herdr-tui"            # または "herdr-core"
routes = ["studio.lan", "studio.tail1234.ts.net"]  # 上から順に試す SSH の接続先
# session = "work"            # herdr の名前つきセッション（既定 default）
# socket = "/home/me/.config/herdr/herdr.sock"   # 省略時は probe で見つける
# bin = "/home/me/.local/bin/herdr"              # 同上
# enabled = false             # 一時的に外す

[notify]
on = ["blocked", "done"]      # 通知する状態
sinks = ["desktop", "bell"]   # "desktop" / "bell" / "command"
command = "curl -d \"$KOOLIE_AGENT: $KOOLIE_STATUS\" ntfy.sh/my-topic"

[[snippet]]
name = "続けて"
text = "続けてください"
```

`command` には環境変数 `KOOLIE_HOST`、`KOOLIE_AGENT`、`KOOLIE_STATUS`、`KOOLIE_TITLE`、`KOOLIE_ID` が渡されます。

その他の環境変数:

| 変数 | 用途 |
|---|---|
| `KOOLIE_CONFIG` | 設定ファイルの場所 |
| `KOOLIE_SSH` | 使う `ssh` コマンド（既定は PATH 上の `ssh`） |
| `XDG_CONFIG_HOME` / `XDG_CACHE_HOME` / `XDG_RUNTIME_DIR` | 設定・キャッシュ・ソケットと status.json の置き場所 |

## チャット表示の前提

チャット表示を使うには、エージェントの会話の記録がどこにあるかを herdr が知っている必要があります。そのマシンで次を実行してから、エージェントを起動し直してください。

```sh
herdr integration install claude
herdr integration install codex
```

## ドキュメント

| ファイル | 内容 |
|---|---|
| [docs/status.md](docs/status.md) | 何を確かめたか、既知の制限、今後の予定 |
| [docs/design.md](docs/design.md) | 構成と、設計で決めたこと |
| [docs/development.md](docs/development.md) | ビルド、テスト、実機での確かめ方 |
| [docs/notes/](docs/notes/) | 開発の記録 |
| [docs/reference/](docs/reference/) | herdr の API スキーマ |
| [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) | 取り込んだデータとコードの出どころとライセンス |
