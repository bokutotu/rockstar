## シンボルの説明

`example` は入力を検証して文字列を返します。

```rust
pub fn example(value: &str) -> Result<String, Error>
```

コードブロックの直後にある説明文です。言語名や外枠と重ならずに表示されます。

```rust
let result = example("日本語 🎸")?;
```

短いコードブロックにも、言語名と本文のための余白があります。

```typescript
const description = "A longer code example wraps within the documentation panel without covering the explanation that follows.";
```

最後の説明文です。**強調**、`inline code`、[リンク](https://example.com)もchatと同じ描画を使います。
