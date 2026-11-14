# 設計

koolie の構成と、作るときに決めたことをまとめます。最初の設計（2026-11-14）を、実装した結果に合わせて書き直したものです。

## 目的

iOS アプリ Heeler for herdr と同じことを、ターミナルの TUI でできるようにします。

- 複数のマシンで動く herdr のエージェントを、1 つの画面にまとめます。
- 対応が必要なエージェントから順に並べ、その場で読む・返事する・端末に入るができるようにします。

既存の TUI（premeq/paddock、herdlord、neyham/herdr-paddock）でできるのは、一覧と端末に入るところまでです。koolie が加えたのは次の 5 つです。

- 入力欄
- チャット表示
- 通知と常駐
- 使用量
- herdr-core 対応

tailnet は、接続方法の 1 つとして扱います。中身はただの SSH の接続先で、Heeler と同じ位置づけです。

## 全体の構成

単一のバイナリで、クレートも 1 つです。非同期処理には tokio を使います。

```
UI (app.rs / ui/ / tui.rs)          ← Session が送る更新だけを見る
  │
Session (session.rs)                 ← ホスト 1 台につき tokio タスク 1 つ
  │
Backend (backend/)                   ← サーバーの種類ごとの API を共通の操作に直す
  │   herdr_tui.rs   改行区切り JSON（Herdr TUI の socket API）
  │   herdr_core.rs  JSON-RPC 2.0 plain モード（herdr-core）
  │
Transport (transport/)               ← ホストまでの経路
      local.rs       このマシン
      ssh.rs         システムの OpenSSH
```

| モジュール | 役割 |
|---|---|
| `model.rs` | Agent、AgentStatus、Inventory の型と並び順。各 Backend の結果はこの型に変換する |
| `config.rs` | 設定ファイルを読み書きし、このマシンの herdr ソケットを自動で見つける |
| `discover/` | ホスト候補の列挙（ssh config / tailscale / herdr machine）と probe（`probe.sh`） |
| `transcript/` | Claude Code / Codex の会話記録を読み、ターン単位にまとめる |
| `usage/` | 会話記録からトークン使用量を集計する。価格表（`pricing.json`）もここ |
| `notify.rs` | 通知するかどうかの判定（Tracker）と、通知の出し先 |
| `watch.rs` | `koolie watch` と `koolie status` |
| `demo.rs` | `--demo` 用の固定データ |

### Backend の共通操作

`backend/mod.rs` の `Backend` trait がすべてです。

- `snapshot`: 全エージェントを取る
- `subscribe`: 変化の通知を受け取る
- `read`: 画面を読む
- `prompt`: 1 回で送る
- `send_keys`: キーを送る
- `insert_text`: Enter なしで入力欄に入れる
- `attach_argv`: 端末に入るためのコマンド
- `capabilities`: その Backend で使える操作

1 つのホスト設定は「SSH の接続先 + サーバーの種類 + セッション名」で 1 件です。同じマシンで Herdr TUI と herdr-core の両方が動いていれば、2 件として登録します（Heeler ADR 0031 と同じ判断です）。

## 接続

### このマシン

次のソケットを自動で登録します。`HERDR_SOCKET_PATH` があれば、それを優先します。

- `~/.config/herdr/herdr.sock`
- `~/.config/herdr/sessions/*/herdr.sock`

リクエストのたびに接続を開きます。イベントの購読だけは 1 本開いたままにします。

### SSH（tailnet もここに含む）

Rust の SSH ライブラリは使わず、システムの `ssh` をそのまま使います。

理由: `~/.ssh/config`、ssh-agent、known_hosts、ProxyJump、Tailscale SSH がすべてそのまま使えるからです。Heeler が libssh2 を使うのは、iOS に `ssh` がないためです。

ホストごとに、親になる接続を 1 本張ります。

```
ssh -M -N -S <ctl> -o BatchMode=yes -o ServerAliveInterval=15 ... <接続先>
```

- herdr のソケットは、必要になった時点で `ssh -S <ctl> -O forward -L <ローカル>:<リモート>` で転送します。転送したローカルのソケットには、このマシンのときと同じコードでつなぎます。
- コマンドは `ssh -S <ctl> <接続先> -- …` で実行します。端末に入るときは `-t` を付けて実行します。どちらも親の接続を使うので、認証は最初の 1 回だけです。
- `BatchMode=yes` を付けているので、裏でパスワードを聞かれて固まることはありません。鍵で入れないホストやホスト鍵が合わないホストは、`Failed { auth }`（画面では「要認証」）にして、自動での再試行をやめます。失敗の種類は `classify_ssh_error` が判定します。
- ローカルのソケット名にはハッシュを使います。ホスト名をファイル名に入れません。
- 経路（routes）が複数あれば、上から順に試します。

### probe

ホストを追加するときに、読み取りだけの sh スクリプト（`discover/probe.sh`）を SSH で 1 回流します。何も起動せず、何もインストールしません。調べるのは次のものです。

- `$HOME` と OS
- herdr の場所とバージョン。PATH になければ `~/.local/bin` なども探します（SSH の非対話シェルは PATH が短いため）。
- herdr のソケット（既定のものと、名前つきのセッション）
- herdr-core の有無と、そのソケット（`core.lock` から読む）
- 相手のマシンに koolie が入っているか

## ホスト 1 台の接続管理（Session）

- 接続の状態は `Connecting` / `Connected` / `Reconnecting` / `Failed { auth }` です。
- 接続の手順は「経路を張る → 購読する → スナップショットを取る」の順です。
- エージェントの一覧は、スナップショットが届くたびに丸ごと置き換えます。
  - herdr は、イベントとスナップショットの順序を保証していません。そのため、イベントは「取り直しの合図」として扱います。
  - イベントを受けたら 150 ms 待ってまとめ、スナップショットを取り直します。保険として、10 秒ごとにも取り直します。
- `pane.agent_status_changed` はペインごとに購読する必要があります。そのため、ペインが増えたり減ったりしたら購読し直します。
- ペインが閉じた直後に購読すると、`pane_not_found` が返ることがあります。これは再試行してよい失敗として扱います。レビュー 1 回目で見つかった重大なバグで、ここでホストが失敗のまま止まっていました。
- 接続に問題がある間は、一覧を空ではなく「不明」として扱います。直前の一覧を薄く残し、操作はできないようにします。
- 再試行は 1 秒から 30 秒までの指数バックオフです。
- 知らない JSON のフィールドは無視します。対応していないメソッドはその操作だけを無効にします（herdr のクライアント向けの指針どおりです）。

`koolie watch` は、失敗したホストに 60 秒ごと（認証の失敗なら 600 秒ごと）につなぎ直します。

## 画面

- **一覧（Console）**
  - 全ホストのエージェントを 1 つのリストにします。
  - 並び順は blocked → done → working → idle → unknown で、同じ状態の中では状態が変わった順です。
  - ユーザーがカーソルを動かした後は、行の順番が変わってもカーソルを同じエージェントに留めます。
- **詳細（Detail）**
  - 上に内容、下に入力欄を置きます。
  - 内容は「画面（Peek）」と「チャット」を切り替えられます。
    - 画面は `pane.read` の ANSI 出力を色つきで表示します。0.7 秒ごとに取り直します。
    - チャットは 1.5 秒ごとに会話の記録の増えた分を読みます。
- **入力欄（Composer）**
  - 下書きしてから 1 回で送る方式です（Heeler と同じ）。
  - エージェントの状態で、送信の動きが変わります。

    | エージェントの状態 | 送信の動き |
    |---|---|
    | idle / done / working | `agent.prompt` |
    | blocked | herdr が `agent.prompt` を断るので、下書きは `pane.send_text` で Enter なしに入れる |

  - 送った文は、会話の記録に現れるまでチャットに仮表示します（最長 60 秒）。
- **入力待ちへの答え**
  - 画面表示のときだけ、`1`-`9` `y` `n` 矢印 `Enter` を `pane.send_keys` でそのまま送ります。
  - 次の 2 つがそろうまではキーを送りません。押し間違いを防ぐためです。
    - 画面を読み込み終わっている
    - 入力待ちになってから 700 ms 経っている
  - 質問を読み取って選択肢のボタンにする機能（Heeler の Blocked Card）は作っていません。
- **端末に入る（Attach）**
  - TUI を一時停止して、本物の端末を前面で動かします（`herdr agent attach <id> --takeover`。herdr-core では `herdr-core attach <t>`）。
  - 埋め込みの端末は作っていません。
  - 入っている間も、ホストの監視と通知は続けます。
- **自分自身のペイン**
  - koolie を herdr のペインで動かしていると、そのペインも一覧に出ます。
  - そのペインには入力を送りません（`HERDR_PANE_ID` で判定する `is_own_pane`）。

## チャット表示

- 会話の内容は、エージェント自身がホストに書く記録ファイルから読みます。herdr の API には会話は含まれていません。
- ファイルの場所は、herdr の `agent_session`（`herdr integration install …` を入れたホストでだけ付きます）から決めます。
  - Claude Code: `~/.claude/projects/<プロジェクト>/<session-id>.jsonl`
  - Codex: `~/.codex/sessions/**/rollout-*.jsonl`
- 読み方:
  - このマシンのファイルは直接読みます。
  - リモートのファイルは、短い sh スクリプトを SSH で実行し、読んだ位置から後ろだけを取ります。値は引数で渡し、スクリプトの文字列には埋め込みません。
  - ファイルが書き直された場合（読んだ位置より短くなった場合）は、最初から読み直します。
- 表示はターン単位です。ターンは、ユーザーの発言 1 つと、それに続くエージェントの作業と回答のまとまりです。終わったターンは作業を畳み、最終回答だけを見せます。ツールの出力本文は保持しません。
- 形式は Heeler の調査メモ（`claude-code-transcript-format.md`、`codex-rollout-format.md`、ADR 0024）を参考にしました。

## 通知と常駐

- エージェントが blocked か done に変わったときに通知します。設定の `notify.on` で変えられます。
  - 接続して最初のスナップショットでは通知しません。
  - 詳細で開いているエージェントについても通知しません。
- 出し先は `desktop`（notify-rust）、`bell`、`command` です。
- `koolie watch` は画面なしで接続を保ち、`$XDG_RUNTIME_DIR/koolie/status.json` を更新し続けます。
- `koolie status` はそのファイルを読みます。
- Heeler のプッシュ中継や E2E 暗号化は作りません。あれは iOS がバックグラウンドで接続を保てないための仕組みです。デスクトップなら、常駐するプロセスで足ります。

## 使用量

- Claude Code と Codex の会話記録を読んで、日・モデル・プロジェクト別に集計し、API の定価で概算します。
- 集計の規則は Heeler の `docs/design/agent-usage.md` と `codex-scanner.js` に合わせています。
  - 同じ応答は二重に数えません。
  - フォークやサブエージェントが親から引き継いだ部分は除きます。
- 読んだ位置を `~/.cache/koolie/usage/` に保存し、次からは増えた分だけ読みます。
- 価格表は、LiteLLM のデータから作ってバイナリに同梱しています。実行時にネットワークへは取りに行きません。
- リモートのホストについては、そのホストに koolie があれば `ssh … koolie usage --json` で取ります。会話記録を SSH 越しに丸ごと読むことはしません（GB 単位になりうるためです。Heeler ADR 0028 と同じ判断です）。
- 利用上限（5 時間枠や週の枠の残り）は扱っていません。

## herdr-core

- プロトコルは JSON-RPC 2.0 で、1 行に 1 メッセージの plain モードです。
  - herdr-core 0.1.21 のスキーマ（protocol generation 1）に合わせて書きました。
  - 使うメソッドは `terminal.list` / `read` / `send` / `keys` と `events.subscribe` です。
- **core を起動しません。**
  - `hc` のサブコマンドはどれも、core が動いていなければ起動してしまいます。そのため一切使わず、ソケットだけで話します。
  - ソケットが見つからなければ「動いていない」と表示します。
  - 理由は Heeler ADR 0031 と同じです。
- `hello` では `generation` だけを送ります。`core` を送ると、koolie が core の仲間として登録されてしまうためです。
- core にはワークスペースもタブもないので、端末を作業ディレクトリごとにまとめて表示します。状態は端末自身の報告を使います（Heeler ADR 0032 を簡単にした規則で、`backend/herdr_core/project.rs` にあります）。
- core には「Enter なしで文字を入れる」API がありません。そのため、入力待ちのエージェントに下書きを入れる機能は使えません。

## 作らなかったもの

| 作らなかったもの | 理由 |
|---|---|
| 埋め込みの端末（VT パーサ） | 一時停止して本物の端末を前面で動かすほうが確実で、実装も小さい |
| 入力待ちの質問を選択肢のボタンにする（Blocked Card） | 画面を見てキー 1 つで答えれば足りる。プログラムごとの解析が必要になる |
| 利用上限の表示 | Claude Code / Codex 自身に問い合わせる必要があり、認証情報は読みたくない |
| Windows ホスト | herdr の Windows 版は名前つきパイプで、SSH のソケット転送が使えない |
| ワークツリーの作成、ファイルの添付 | 最初の版の範囲外 |

## 主な依存クレート

| 用途 | クレート |
|---|---|
| 画面 | ratatui 0.30、crossterm 0.29、ansi-to-tui |
| 非同期 | tokio |
| JSON / 設定 | serde、serde_json、toml |
| CLI | clap |
| 通知 | notify-rust（Linux は D-Bus、macOS は通知センター） |
| 文字幅 | unicode-width（入力欄は自前の `ui/text.rs`） |

## 参考にしたもの

- herdr の socket API: <https://herdr.dev/docs/socket-api/>。`herdr api schema --json` の出力は [reference/](reference/) にあります。
- Heeler（Apache-2.0）: `CONTEXT.md`、ADR 0011 / 0024 / 0028 / 0031 / 0032、`docs/research/*-transcript-format.md`、`docs/design/agent-usage.md`
- premeq/paddock（MIT）: イベントの購読と購読し直しのやり方
- herdr-core: `schema/README.md`、`schema/hc.openrpc.json`
