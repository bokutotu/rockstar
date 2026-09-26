Write docs only when asked.
Keep it simple; avoid overengineering.

## Tests

Keep Arrange, Act, and Assert in visibly separate blocks:

- **Arrange:** Define inputs and the complete expected value explicitly, without calling the code under test.
- **Act:** Bind the result as `actual` (or an `IO` action as `action`).
- **Assert:** Use Hspec's ``actual `shouldBe` expected`` (or ``action `shouldReturn` expected`` for `IO`).

Compare whole values (records, lists, tuples, or `Data.Aeson.Value`), not individual fields or selected elements. Even checking every field separately is not a whole-value comparison.

Example: adding an item updates both the item list and the total.

```haskell
import Test.Hspec

data Cart = Cart {items :: [String], total :: Int} deriving (Eq, Show)

addItem :: String -> Int -> Cart -> Cart
addItem item price cart = Cart {items = items cart <> [item], total = total cart + price}

spec :: Spec
spec = describe "addItem" $ do
    it "bad: mixes setup with execution and asserts field by field" $ do
        let input = Cart {items = ["apple"], total = 100}
            item = "orange"
            price = 80
            actual = addItem item price input

        items actual `shouldBe` ["apple", "orange"]
        total actual `shouldBe` 180

    it "good: separates Arrange / Act / Assert and compares the whole cart" $ do
        -- Arrange
        let input = Cart {items = ["apple"], total = 100}
            item = "orange"
            price = 80
            expected = Cart {items = ["apple", "orange"], total = 180}

        -- Act
        let actual = addItem item price input

        -- Assert
        actual `shouldBe` expected
```

## Coding Style

Keep Haskell expressions and declarations on one line whenever the formatter allows; avoid unnecessary wrapping.
