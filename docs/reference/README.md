# 参考資料

| ファイル | 内容 |
|---|---|
| `herdr-api-schema-0.9.5.json` | herdr 0.9.5 の socket API の JSON スキーマ（`herdr api schema --json` の出力） |

herdr を新しい版に上げたら、スキーマを取り直して違いを確かめます。

```sh
herdr api schema --json > docs/reference/herdr-api-schema-$(herdr --version | awk '{print $2}').json
```

herdr-core のスキーマは保存していません。必要になったら、herdr-core の `schema/hc.openrpc.json`（koolie は 0.1.21 に合わせて書いた）を見てください。
