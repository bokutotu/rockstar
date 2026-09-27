module ChatSpec (spec) where

import           Control.Exception      (AsyncException (UserInterrupt),
                                         throwIO, try, tryJust)
import           Control.Monad          (forM_)
import           Data.Aeson
import           Data.IORef
import           Data.Text              (Text)
import           Rockstar.Chat.Internal
import           Rockstar.Chat.Types
import           Rockstar.Codex
import qualified Rockstar.Http          as Http
import           Rockstar.Interrupt     (Interrupted (..))
import           Test.Hspec
import           TestSupport

spec :: Spec
spec = describe "chat" $ do
    it "stages input without modifying the original conversation" $ do
        -- Arrange
        let original = Conversation defaultModel []
            input = "hello"
            expected =
                ( Conversation
                    "gpt-6-astra"
                    [ object
                        [ "type" .= ("message" :: Text)
                        , "role" .= ("user" :: Text)
                        , "content" .= [object ["type" .= ("input_text" :: Text), "text" .= ("hello" :: Text)]]
                        ]
                    ]
                , Conversation "gpt-6-astra" []
                )

        -- Act
        let actual = (stageTurn original input, original)

        -- Assert
        actual `shouldBe` expected

    it "displays the completed reply once and removes terminal controls only from display" $ Http.withManager $ \manager -> do
        -- Arrange
        display <- newIORef []
        let text = "hello\ESC[31m"
            original = Conversation defaultModel []
            body =
                sse
                    [ object ["type" .= ("response.output_text.delta" :: Text), "delta" .= ("partial" :: Text)]
                    , completeEvent [message text]
                    ]
            expected =
                ( Right $
                    Conversation
                        "gpt-6-astra"
                        [ object
                            [ "type" .= ("message" :: Text)
                            , "role" .= ("user" :: Text)
                            , "content" .= [object ["type" .= ("input_text" :: Text), "text" .= ("hi" :: Text)]]
                            ]
                        , message text
                        ]
                , ["hello[31m"]
                )

        -- Act
        let action = withServer (streamApp body) $ \url -> do
                let send conversation = try $ fetchReplyAt url manager (credentials 4600) conversation
                next <- sendTurn send original "hi" (\text' -> modifyIORef' display (<> [text']))
                displayed <- readIORef display
                pure (next, displayed)

        -- Assert
        action `shouldReturn` expected

    it "does not display partial text or return a conversation for unsuccessful replies" $ Http.withManager $ \manager -> do
        let cases =
                [ ([object ["type" .= ("response.failed" :: Text)]], GenerationFailed)
                , ([object ["type" .= ("response.incomplete" :: Text)]], GenerationIncomplete)
                , ([], ProtocolError "stream ended without a completed response")
                ,
                    ( [completeEvent [object ["type" .= ("function_call" :: Text)]]]
                    , ProtocolError "unsupported output item; this harness has no tools yet"
                    )
                ]
        forM_ cases $ \(events, error') -> do
            -- Arrange
            display <- newIORef []
            let original = Conversation defaultModel [message "previous"]
                partial = object ["type" .= ("response.output_text.delta" :: Text), "delta" .= ("partial" :: Text)]
                expected = (Left error', [])

            -- Act
            let action = withServer (streamApp $ sse (partial : events)) $ \url -> do
                    let send conversation = try $ fetchReplyAt url manager (credentials 4600) conversation
                    result <- sendTurn send original "hi" (\text -> modifyIORef' display (<> [text]))
                    displayed <- readIORef display
                    pure (result, displayed)

            -- Assert
            action `shouldReturn` expected

    it "does not display or return a conversation when interrupted" $ do
        -- Arrange
        display <- newIORef []
        let original = Conversation defaultModel [message "previous"]
            cancelled _ = throwIO UserInterrupt
            expected = (Left "Interrupted; the incomplete turn was not saved", [])

        -- Act
        let action = do
                result <-
                    tryJust (\(Interrupted reason) -> Just reason) $
                        sendTurn cancelled original "hi" (\text -> modifyIORef' display (<> [text]))
                displayed <- readIORef display
                pure (result, displayed)

        -- Assert
        action `shouldReturn` expected

    it "allows newlines and tabs but removes other control characters" $ do
        -- Arrange
        let input = "hello\NUL\ESC\DEL\x85\n\tworld"
            expected = "hello\n\tworld"

        -- Act
        let actual = sanitizeOutput input

        -- Assert
        actual `shouldBe` expected
