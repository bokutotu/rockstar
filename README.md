# Rockstar

pi とのチャットとソースコード編集を、分割したタイルで扱う GPUI アプリです。
起動ディレクトリをワークスペースとして使います。

```sh
nix develop --command cargo run
```

起動時と分割時の新しいタイルは **plain** です。ファイル編集には pi は不要で、チャットを作成するときに接続します。

## タイルとファイルを開く

Normal 時の操作です。`\ff` はバックスラッシュ、`f`、`f` の順に押します。

| 操作 | 結果 |
|---|---|
| `:chat` Enter | 現在のタイルを新しい chat に変更。すでに chat なら維持 |
| `:new` Enter | 現在のタイルに新しいチャットを作成 |
| `\ff` | ファジーファインダー。選んだファイルを現在のタイルに表示 |
| `Ctrl-b` | 左端のファイルツリーを開閉 |
| `ss` / `sv` | 下／右に plain タイルを追加 |
| `sh` / `sj` / `sk` / `sl` | 左／下／上／右のタイルへ移動 |
| `Ctrl-n` / `Ctrl-p` | chat ではセッション、fileview では開いたファイルを切り替え。plain では既存チャットを表示 |
| `:close` または `:q` | 現在のタイルを閉じる。最後のタイルは保持 |
| `:quit` または `:qa` | ウィンドウを閉じる。未保存ファイルがあれば確認 |

検索では `Ctrl-n/p` または上下キーで候補を選び、Enter で開き、Esc でキャンセルします。
Git 管理下では追跡済みファイルと、無視されていない未追跡ファイルを対象にします。
Git 管理外でも ignore 規則を適用します。

ツリーでは `j/k` または上下キーで移動、`h/l` または左右キーで折り畳み・展開、Enter で開きます。
ドットファイルは非表示です。`r` で一覧を再読み込みできます。

## Fileview

`~/nixos-config` の Neovim 設定を参考にしたキー操作です。Neovim 本体やプラグインを起動する方式ではありません。

| 操作 | 結果 |
|---|---|
| `h/j/k/l`、`w/b/e`、`0/^/$`、`gg/G` | カーソル移動。`10j` など回数指定にも対応 |
| `i/a/I/A/o/O` | Insert に入り、入力・行追加 |
| Esc | 補完候補を閉じる。候補がなければ Normal に戻る。Normal 時には検索ハイライトを解除 |
| `v` / `V` | 文字／行の Visual 選択 |
| `d/c/y` + 移動、`dd/cc/yy` | 削除／変更／コピー |
| Visual 中の `d/c/y` | 選択範囲を削除／変更／コピー |
| `x`、`D`、`C` | 文字削除、行末まで削除／変更 |
| `p/P` | クリップボードから貼り付け |
| `u` / `Ctrl-r` | Undo／Redo |
| `>>/<<`、Visual 中の `>/<` | インデント／インデント解除 |
| `/` → 検索語 → Enter、`n/N` | 大文字を含むときだけ大小文字を区別して検索、次／前の一致へ移動 |
| `:w`、`Space w` | 保存 |
| Insert 中の `jk` | Normal に戻って保存 |
| Insert 中の `jjj` | Normal に戻って次の行へ移動 |

`jk` / `jjj` はキー間隔が1秒以内のときに適用します。
相対行番号、利用可能な言語のシンタックスハイライト、言語別のスペースインデントを表示・適用します。
矩形選択、マクロ、テキストオブジェクト、Neovim のプラグイン機能は対象外です。

同じファイルの本文と Undo 履歴はタイル間で共有し、カーソル・スクロール位置はタイルごとに保持します。
ファイルやチャットを切り替えたりタイルを閉じたりしても、内容はアプリの起動中保持されます。

UTF-8 の通常ファイルが対象です。BOM・CRLF・ファイルのパーミッションを保存時に保持し、保存前に外部変更を検出した場合は通知します。
外部変更を自動でマージする機能や、終了後の未保存バッファ復元はありません。

### LSP

プロジェクトの Nix devShell に言語サーバーを用意し、その環境から Rockstar を起動します。
起動ディレクトリをプロジェクトルートとし、継承した `PATH` から次のコマンドを実行します。
アプリ自身が `nix develop` を呼んだり、サーバーをインストールしたりすることはありません。

| 言語 | コマンド |
|---|---|
| Rust | `rust-analyzer` |
| Haskell | `haskell-language-server-wrapper --lsp` |
| TypeScript / TSX | `typescript-language-server --stdio` |
| Terraform | `terraform-ls serve` |
| C / C++ / CUDA | `ccls` |
| Nix | `nixd` |

このリポジトリの devShell には `rust-analyzer` と `nixd` を含めています。
C 系のプロジェクトでは `compile_commands.json` または `.ccls`、Haskell ではプロジェクトに合う GHC／cradle も必要です。
サーバーが見つからない場合は通知とステータス欄にエラーを表示します。通常の編集・保存は続けられます。

| 操作 | 結果 |
|---|---|
| `F12` | 定義へ移動。複数候補があれば一覧から選択 |
| `F11` | incoming／outgoing の Call hierarchy を表示 |
| 階層内の `Tab`、↑/↓、←/→、Enter、Esc | 方向切り替え、選択、折り畳み／展開、移動、閉じる |
| Normal 中の `K`、マウスホバー | ドキュメント表示 |
| Insert 中の入力、`Ctrl-Space` | 自動補完／補完候補を開く |
| 補完中の ↑/↓ または `Ctrl-n/p`、Enter、Esc | 候補選択、確定、閉じる |
| コードの上にある Code lens をクリック | 参照先の表示など、その注釈の操作を実行 |

診断、カーソル位置のシンボル強調、semantic tokens、inlay hints、Code lens は、サーバーが対応していれば自動表示します。
Tree-sitter の構文色分けに semantic tokens を重ね、hint／lens は本文・コピー内容・保存ファイルには含めません。
TypeScript の hints／reference lenses と Rust の reference lenses は接続時に有効化します。
`ccls` が標準の inlay hints に非対応の場合、その注釈は表示しません。

同じ文書の同期はタイル間で共有し、シンボル強調は各タイルのカーソルに従います。
編集前の文書に対する応答は破棄し、補完の追加編集は元の文書に対してまとめて適用します。自動保存はしません。
保存成功時だけ `didSave` を通知し、終了時にはサーバーも停止します。

Run／Debug や VS Code 専用の未対応コマンドは通知します。rename、formatting、code actions、
サーバーから要求された workspace edits、クライアント側のファイル監視は現在の対象外です。
semantic token の modifier 別スタイルと、hint 内のリンク・ツールチップも未対応です。

## Chat

`i` または入力欄のクリックで入力します。Enter は改行、`Ctrl-Enter` は送信、Esc は Normal に戻ります。
`@` からワークスペース内のファイルを検索して、プロンプトに参照を挿入できます。

テストの実行方法は [tests/README.md](tests/README.md) を参照してください。
