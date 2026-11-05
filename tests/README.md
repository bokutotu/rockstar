# Tests

## 本番 UI を通す E2E

```sh
nix develop --command cargo test -- --test-threads=1
nix develop --command cargo clippy --all-targets -- -D warnings
```

`src/e2e_tests.rs` とその子モジュールで本番ウィンドウを開き、キー入力・クリックを送ります。
内部の編集関数を直接呼ぶ代わりに、表示内容・選択された項目・フォーカス・保存されたファイルの内容を検証します。
ファイルと Git リポジトリは一時ディレクトリに実際に作成し、pi 接続をモックに置き換えます。
通常の UI テストでは LSP を無効化し、言語サーバーの有無や応答時間に依存しないようにしています。

`src/e2e_tests/files/visual.rs` はソースの色分け、相対行番号、ツリーを画像で検証します。
通常のテストでは参照画像を書き換えません。意図した表示変更のときだけ次を実行し、画像をレビューしてください。

```sh
nix develop --command cargo test update_fileview_snapshot -- --ignored
```

差分発生時の実画像は `target/visual-diffs/fileview/actual.png` に出力します。

## LSP

`src/e2e_tests/files/lsp.rs` は `tests/mock_lsp.py` を子プロセスとして起動します。
置き換えるのはサーバーだけで、stdio 通信、バッファ同期、GPUI の補完・描画・キー処理は本番コードを使います。
Python 3 が必要です（この devShell と Nix のテスト環境に含まれます）。

- F11 の両方向・展開・移動、F12 の単一／複数候補、K のホバー要求
- ネイティブ補完と追加編集、Undo、複数タイルで一度だけ同期されること
- full／incremental 同期、Unicode の位置変換、保存成功時の通知
- 古い応答の破棄、未導入サーバー、未対応機能
- 注釈の描画、Code lens のクリック、注釈後のマウス座標、コピー・保存内容

`src/lsp/transport/tests.rs` は応答の順序逆転、キャンセル、異常終了、stdin を読まなくなったサーバーの停止も確認します。
注釈と診断の画像は `tests/visual/lsp.png`、不一致時の実画像は `target/visual-diffs/lsp/actual.png` です。
意図した表示変更のときだけ参照画像を更新し、レビューします。

```sh
nix develop --command cargo test update_lsp_snapshot -- --ignored
```

実サーバーのテストは通常の実行から除外しています。対象プロジェクト用の devShell に入り、次を実行してください。
初期化、ホバー・定義と、対応を表明した補完・シンボル強調・階層・hint・lens・semantic tokens の要求を確認します。
サーバーごとに返す情報は異なり、すべての要求で非空の結果が得られることは条件にしていません。

```sh
# 値: rust / haskell / typescript / terraform / c / cpp / cuda / nix
ROCKSTAR_LSP_LANGUAGE=rust cargo test real_language_servers -- --ignored --nocapture
# 全8言語のサーバーと関連ツールが PATH にある場合
cargo test real_language_servers -- --ignored --nocapture
```

CUDA の fixture はホスト関数のみです。CUDA SDK、デバイスコード、各実プロジェクトのビルド設定の検証は別途必要です。

## 実際の X11・Fcitx5・Mozc

```sh
nix develop --command cargo build
python3 tests/ime_x11.py
# stdio サーバーを使い、hint／lens 周辺の座標と保存内容も検証
python3 tests/ime_x11.py --lsp
# 組み合わせを絞る場合
python3 tests/ime_x11.py --scale 1 --preedit on
```

必要なコマンドは `Xvfb`、`dbus-run-session`、`xdotool`、`fcitx5`、`fcitx5-remote` です。
Fcitx5 に Mozc が必要です。

テスト専用の X11 ディスプレイ・D-Bus・HOME・ワークスペースを作成するため、普段使っているウィンドウや設定を操作しません。
pi は記録用スタブを使い、外部 API には接続しません。

チャットの改行・リサイズ・折り返し・スクロール後の候補位置と明示的な送信、fileview への切り替え・日本語入力・UTF-8 保存を確認します。
`--lsp` では注釈あり／なしの候補位置を比較し、preedit で注釈が消える場合も座標が追従することを確認します。
倍率1／2と on-the-spot 無効／有効の4通りを実行します。辞書学習による候補順序の変化を避けるため、確定する文字列は F6 で明示的にひらがなを選びます。
