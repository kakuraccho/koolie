# 他のプロジェクトから取り込んだもの

koolie 自体は MIT License です（[LICENSE](LICENSE)）。ただし、次のものはそれぞれの出どころのライセンスに従います。

## 価格表（`src/usage/pricing.json`）

- **出どころ**: Heeler に同梱されている価格表を、そのままコピーしました。
  - コピー元: <https://github.com/ZingerLittleBee/Heeler/blob/2ca0abc112a37aa4a3f241785b5e90c43411d44f/plugin/src/usage/pricing.json>
  - 価格表の版（`pricingVersion`）: 2026-10-30
  - Heeler はこの表を、LiteLLM の `model_prices_and_context_window.json` から作っています。
- **ライセンス**
  - 元のデータは LiteLLM（Copyright (c) 2023 Berri AI）の MIT License です。全文は [`src/usage/pricing.LICENSE`](src/usage/pricing.LICENSE) にあります。
  - ファイル自体は Heeler からのコピーなので、下の Heeler の Apache License 2.0 も当てはまります。
- **追加した価格**: この表の版より後に出たモデルの価格を、`src/usage/prices.rs` の `SUPPLEMENT` に足しています。

## Heeler（Apache License 2.0）

<https://github.com/ZingerLittleBee/Heeler>

**移植したコード**

- `src/usage/codex.rs`: Heeler の `plugin/src/usage/codex-scanner.js` を移植したもの
- `src/usage/claude.rs`: Heeler の `docs/design/agent-usage.md` に書かれた数え方に従ったもの

**参考にした文書**

設計と実装で参考にしました。

- `CONTEXT.md`
- ADR 0011 / 0024 / 0028 / 0031 / 0032
- `docs/research/claude-code-transcript-format.md`
- `docs/research/codex-rollout-format.md`

**ライセンス**

- 全文: [`licenses/Heeler-Apache-2.0.txt`](licenses/Heeler-Apache-2.0.txt)。コミット `2ca0abc` の `LICENSE` をそのままコピーしました。
- Heeler には `NOTICE` ファイルがありません（同じコミットで確認）。
- 移植したファイルには、移植元と、Rust に書き直して変更したことを先頭のコメントに書いています。

## herdr の API スキーマ（`docs/reference/herdr-api-schema-0.9.5.json`）

herdr 0.9.5 で `herdr api schema --json` を実行した出力を、そのまま保存したものです。開発のときの参照用で、バイナリには入っていません。
