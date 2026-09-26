module ChatSpec (spec) where

import           Control.Exception      (AsyncException (UserInterrupt),
                                         throwIO, try, tryJust)
import           Control.Monad          (forM_)
import           Data.Aeson
import           Data.IORef
import           Data.Text              (Text)
import           Rockstar.Auth.Types    (toChatAuth)
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
        let original = Conversation defaultModel []
        stageTurn original "hello"
            `shouldBe` Conversation
                "gpt-6-astra"
                [ object
                    [ "type" .= ("message" :: Text)
                    , "role" .= ("user" :: Text)
                    , "content" .= [object ["type" .= ("input_text" :: Text), "text" .= ("hello" :: Text)]]
                    ]
                ]
        original `shouldBe` Conversation "gpt-6-astra" []

    it "displays the completed reply once and removes terminal controls only from display" $ Http.withManager $ \manager -> do
        display <- newIORef []
        let text = "hello\ESC[31m"
            original = Conversation defaultModel []
            body =
                sse
                    [ object ["type" .= ("response.output_text.delta" :: Text), "delta" .= ("partial" :: Text)]
                    , object ["type" .= ("response.output_text.delta" :: Text), "delta" .= (" text" :: Text)]
                    , completeEvent [message text]
                    ]
            expected =
                ( Conversation
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
        withServer (streamApp body) $ \url -> do
            next <-
                sendTurn
                    (fetchReplyAt url manager (toChatAuth $ credentials 4600))
                    original
                    "hi"
                    (\text' -> modifyIORef' display (<> [text']))
            displayed <- readIORef display
            (next, displayed) `shouldBe` expected

    it "does not display partial text or return a conversation for unsuccessful replies" $ Http.withManager $ \manager -> do
        let original = Conversation defaultModel [message "previous"]
            partial = object ["type" .= ("response.output_text.delta" :: Text), "delta" .= ("partial" :: Text)]
            cases =
                [ ([object ["type" .= ("response.failed" :: Text)]], GenerationFailed)
                , ([object ["type" .= ("response.incomplete" :: Text)]], GenerationIncomplete)
                , ([], ProtocolError "stream ended without a completed response")
                ,
                    ( [completeEvent [object ["type" .= ("function_call" :: Text)]]]
                    , ProtocolError "unsupported output item; this harness has no tools yet"
                    )
                ]
        forM_ cases $ \(events, expected) -> do
            display <- newIORef []
            result <- withServer (streamApp $ sse (partial : events)) $ \url ->
                try
                    ( sendTurn
                        (fetchReplyAt url manager (toChatAuth $ credentials 4600))
                        original
                        "hi"
                        (\text -> modifyIORef' display (<> [text]))
                    )
            displayed <- readIORef display
            (result, displayed) `shouldBe` (Left expected, [])

    it "does not display or return a conversation when interrupted" $ do
        display <- newIORef []
        let original = Conversation defaultModel [message "previous"]
            cancelled _ = throwIO UserInterrupt
        result <-
            tryJust (\(Interrupted reason) -> Just reason) $
                sendTurn cancelled original "hi" (\text -> modifyIORef' display (<> [text]))
        displayed <- readIORef display
        (result, displayed) `shouldBe` (Left "Interrupted; the incomplete turn was not saved", [])

    it "allows newlines and tabs but removes other control characters" $
        sanitizeOutput "hello\NUL\ESC\DEL\x85\n\tworld" `shouldBe` "hello\n\tworld"
