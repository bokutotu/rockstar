module CodexSpec (spec) where

import           Control.Exception       (try)
import           Control.Monad           (forM_)
import           Data.Aeson
import qualified Data.ByteString         as BS
import qualified Data.ByteString.Builder as Builder
import qualified Data.ByteString.Char8   as BS8
import qualified Data.ByteString.Lazy    as LBS
import           Data.IORef
import           Data.List               (sort)
import           Data.Text               (Text)
import qualified Data.Text               as Text
import           Data.Text.Encoding      (encodeUtf8)
import           Network.HTTP.Types
import           Network.Wai
import           Rockstar.Auth.Types     (toChatAuth)
import           Rockstar.Chat.Types
import           Rockstar.Codex
import qualified Rockstar.Http           as Http
import           Test.Hspec
import           TestSupport

spec :: Spec
spec = describe "Codex transport" $ do
    it "sends complete headers and settings, preserves reasoning, and decodes split UTF-8" $ Http.withManager $ \manager -> do
        received <- newIORef []
        let reasoning =
                object
                    [ "type" .= ("reasoning" :: Text)
                    , "id" .= ("rs_test" :: Text)
                    , "summary" .= ([] :: [Value])
                    , "encrypted_content" .= ("opaque-reasoning" :: Text)
                    ]
            output = [reasoning, message "こんにちは"]
            body =
                sse
                    [ object ["type" .= ("response.output_text.delta" :: Text), "delta" .= ("こんにちは" :: Text)]
                    , completeEvent output
                    ]
            app request respond = do
                payload <- strictRequestBody request
                json <- either fail pure (eitherDecode' payload)
                modifyIORef' received (<> [(sort $ requestHeaders request, json)])
                respond $ responseStream status200 [("Content-Type", "text/event-stream; charset=utf-8")] $ \write flush ->
                    forM_ (LBS.unpack body) $ \byte -> write (Builder.word8 byte) >> flush
            auth = toChatAuth (credentials 4600)
            conversation = stageTurn (Conversation defaultModel []) "hello"
            expectedReply = Reply output
            next = stageTurn (commitTurn conversation expectedReply) "continue"
        withServer app $ \url -> do
            fetchReplyAt url manager auth conversation `shouldReturn` expectedReply
            fetchReplyAt url manager auth next `shouldReturn` expectedReply
            let firstBody =
                    object
                        [ "model" .= ("gpt-6-astra" :: Text)
                        , "instructions" .= ("You are a helpful assistant." :: Text)
                        , "input"
                            .= [ object
                                    [ "type" .= ("message" :: Text)
                                    , "role" .= ("user" :: Text)
                                    , "content" .= [object ["type" .= ("input_text" :: Text), "text" .= ("hello" :: Text)]]
                                    ]
                               ]
                        , "stream" .= True
                        , "store" .= False
                        , "reasoning" .= object ["effort" .= ("max" :: Text), "summary" .= ("auto" :: Text)]
                        , "service_tier" .= ("priority" :: Text)
                        , "include" .= (["reasoning.encrypted_content"] :: [Text])
                        , "tools" .= ([] :: [Value])
                        , "tool_choice" .= ("none" :: Text)
                        ]
                secondBody =
                    object
                        [ "model" .= ("gpt-6-astra" :: Text)
                        , "instructions" .= ("You are a helpful assistant." :: Text)
                        , "input"
                            .= [ object
                                    [ "type" .= ("message" :: Text)
                                    , "role" .= ("user" :: Text)
                                    , "content" .= [object ["type" .= ("input_text" :: Text), "text" .= ("hello" :: Text)]]
                                    ]
                               , reasoning
                               , message "こんにちは"
                               , object
                                    [ "type" .= ("message" :: Text)
                                    , "role" .= ("user" :: Text)
                                    , "content" .= [object ["type" .= ("input_text" :: Text), "text" .= ("continue" :: Text)]]
                                    ]
                               ]
                        , "stream" .= True
                        , "store" .= False
                        , "reasoning" .= object ["effort" .= ("max" :: Text), "summary" .= ("auto" :: Text)]
                        , "service_tier" .= ("priority" :: Text)
                        , "include" .= (["reasoning.encrypted_content"] :: [Text])
                        , "tools" .= ([] :: [Value])
                        , "tool_choice" .= ("none" :: Text)
                        ]
                expectedHeaders payload =
                    sort
                        [ ("Accept", "text/event-stream")
                        , ("Accept-Encoding", "gzip")
                        , ("Authorization", "Bearer " <> encodeUtf8 (accessToken auth))
                        , ("ChatGPT-Account-Id", "test-account")
                        , ("Content-Type", "application/json")
                        , ("Content-Length", BS8.pack $ show $ LBS.length $ encode payload)
                        , ("Host", BS8.pack $ drop 7 url)
                        , ("OpenAI-Beta", "responses=experimental")
                        , ("originator", "rockstar")
                        , ("User-Agent", "rockstar/0.1.0")
                        ]
            readIORef received
                `shouldReturn` [(expectedHeaders firstBody, firstBody), (expectedHeaders secondBody, secondBody)]

    it "accepts headerless SSE and collects ordered output when completed output is empty" $ Http.withManager $ \manager -> do
        let reasoning = object ["type" .= ("reasoning" :: Text), "encrypted_content" .= ("opaque-reasoning" :: Text)]
            body = sse [itemDone 1 (message "こんにちは"), itemDone 0 reasoning, completeEvent []]
            app _ respond = respond (responseLBS status200 [] body)
            expected = Reply [reasoning, message "こんにちは"]
        withServer app $ \url ->
            fetchReplyAt url manager (toChatAuth $ credentials 4600) (Conversation defaultModel [])
                `shouldReturn` expected

    it "supports output-item fallback and response.done" $ Http.withManager $ \manager -> do
        let body =
                sse
                    [ itemDone 0 (message "hello")
                    , object
                        ["type" .= ("response.done" :: Text), "response" .= object ["status" .= ("completed" :: Text)]]
                    ]
        withServer (streamApp body) $ \url ->
            fetchReplyAt url manager (toChatAuth $ credentials 4600) (Conversation defaultModel [])
                `shouldReturn` Reply [message "hello"]

    it "ignores text and refusal deltas and returns only the completed output" $ Http.withManager $ \manager -> do
        let body =
                sse
                    [ object ["type" .= ("response.output_text.delta" :: Text), "delta" .= ("partial text" :: Text)]
                    , object ["type" .= ("response.refusal.delta" :: Text), "delta" .= ("partial refusal" :: Text)]
                    , completeEvent [message "final answer"]
                    ]
        withServer (streamApp body) $ \url ->
            fetchReplyAt url manager (toChatAuth $ credentials 4600) (Conversation defaultModel [])
                `shouldReturn` Reply [message "final answer"]

    it "frames comments, BOM, multi-line data, and all SSE newline styles" $ Http.withManager $ \manager ->
        forM_ ["\n", "\r", "\r\n"] $ \newline -> do
            let body =
                    "\239\187\191: comment"
                        <> newline
                        <> "event: ignored"
                        <> newline
                        <> "data: {\"type\":\"response.completed\","
                        <> newline
                        <> "data: \"response\":{\"status\":\"completed\",\"output\":[]}}"
                        <> newline
                        <> newline
                app _ respond = respond $ responseStream status200 [("Content-Type", "text/event-stream")] $ \write flush ->
                    forM_ (LBS.unpack body) $ \byte -> write (Builder.word8 byte) >> flush
            withServer app $ \url ->
                fetchReplyAt url manager (toChatAuth $ credentials 4600) (Conversation defaultModel [])
                    `shouldReturn` Reply []

    it "rejects failed, incomplete, and malformed responses even without Content-Type" $ Http.withManager $ \manager -> do
        let delta = object ["type" .= ("response.output_text.delta" :: Text), "delta" .= ("partial" :: Text)]
            cases =
                [ (sse [delta], ProtocolError "stream ended without a completed response")
                ,
                    ( sse [object ["type" .= ("response.failed" :: Text), "secret" .= ("do-not-print" :: Text)]]
                    , GenerationFailed
                    )
                , (sse [object ["type" .= ("error" :: Text)]], GenerationFailed)
                , (sse [object ["type" .= ("response.incomplete" :: Text)]], GenerationIncomplete)
                , ("data: not-json-secret\n\n", ProtocolError "invalid event JSON")
                , ("data: [DONE]\n\n", ProtocolError "stream ended without a completed response")
                , ("<html>not an SSE response</html>", ProtocolError "stream ended without a completed response")
                , (sse [object []], ProtocolError "missing event type")
                ,
                    ( sse [itemDone 1 (message "hello"), completeEvent []]
                    , ProtocolError "missing completed output items"
                    )
                ,
                    ( sse [object ["type" .= ("response.completed" :: Text)]]
                    , ProtocolError "missing completed response"
                    )
                ,
                    ( sse
                        [ object
                            ["type" .= ("response.done" :: Text), "response" .= object ["status" .= ("completed" :: Text)]]
                        ]
                    , ProtocolError "missing output in completed response"
                    )
                ]
                    <> [ ( sse [object ["type" .= ("response.completed" :: Text), "response" .= object ["status" .= status']]]
                         , expected
                         )
                       | (status', expected) <-
                            [ ("failed" :: Text, GenerationFailed)
                            , ("cancelled", GenerationFailed)
                            , ("incomplete", GenerationIncomplete)
                            , ("unknown", ProtocolError "missing or unexpected terminal status")
                            ]
                       ]
        forM_ cases $ \(body, expected) ->
            forM_ [[], [("Content-Type", "text/event-stream")]] $ \headers -> do
                let app _ respond = respond (responseLBS status200 headers body)
                withServer app $ \url ->
                    try (fetchReplyAt url manager (toChatAuth $ credentials 4600) (Conversation defaultModel []))
                        `shouldReturn` Left expected

    it "bounds partial events before a newline arrives" $ Http.withManager $ \manager -> do
        let body = "data: " <> LBS.fromStrict (BS.replicate (16 * 1024 * 1024) 120)
        withServer (streamApp body) $ \url ->
            try (fetchReplyAt url manager (toChatAuth $ credentials 4600) (Conversation defaultModel []))
                `shouldReturn` Left (ProtocolError "event too large")

    it "rejects HTTP errors without retrying or printing response bodies" $ Http.withManager $ \manager ->
        forM_
            [ (status401, AuthenticationRejected)
            , (status403, AccessDenied)
            , (status429, RateLimited)
            , (status502, RequestRejected 502)
            ]
            $ \(status', expected) -> do
                calls <- newIORef (0 :: Int)
                let app _ respond = modifyIORef' calls (+ 1) >> respond (responseLBS status' [] "secret diagnostics")
                result <- withServer app $ \url ->
                    try (fetchReplyAt url manager (toChatAuth $ credentials 4600) (Conversation defaultModel []))
                count <- readIORef calls
                (result, count) `shouldBe` (Left expected, 1)

    it "rejects explicitly non-SSE Content-Types" $ Http.withManager $ \manager ->
        forM_ ["application/json", "text/html", ""] $ \contentType -> do
            let app _ respond = respond (responseLBS status200 [("Content-Type", contentType)] (sse [completeEvent []]))
            withServer app $ \url ->
                try (fetchReplyAt url manager (toChatAuth $ credentials 4600) (Conversation defaultModel []))
                    `shouldReturn` Left UnexpectedContentType

    it "prevents authentication-header injection before sending a request" $ Http.withManager $ \manager -> do
        calls <- newIORef (0 :: Int)
        let app _ respond = modifyIORef' calls (+ 1) >> respond (responseLBS status200 [] "")
        result <- withServer app $ \url ->
            try
                ( fetchReplyAt
                    url
                    manager
                    (ChatAuth "bad\r\nInjected: yes" "test-account")
                    (Conversation defaultModel [])
                )
        count <- readIORef calls
        (result, count) `shouldBe` (Left (ProtocolError "invalid authentication header"), 0)

    it "never forwards access tokens through redirects" $ Http.withManager $ \manager -> do
        calls <- newIORef (0 :: Int)
        let destination _ respond = modifyIORef' calls (+ 1) >> respond (responseLBS status200 [] "")
        result <- withServer destination $ \target -> do
            let origin _ respond = respond (responseLBS status307 [("Location", encodeUtf8 $ Text.pack target)] "")
            withServer origin $ \url ->
                try (fetchReplyAt url manager (toChatAuth $ credentials 4600) (Conversation defaultModel []))
        count <- readIORef calls
        (result, count) `shouldBe` (Left (RequestRejected 307), 0)

    describe "replyText" $ do
        it "extracts text and refusals, not reasoning" $ do
            let reasoning = object ["type" .= ("reasoning" :: Text), "encrypted_content" .= ("opaque-reasoning" :: Text)]
                refusal =
                    object
                        [ "type" .= ("message" :: Text)
                        , "content" .= [object ["type" .= ("refusal" :: Text), "refusal" .= ("no" :: Text)]]
                        ]
                reply = Reply [reasoning, message "hello\n", refusal]
            replyText reply `shouldBe` Right "hello\nno"
            replyText (Reply []) `shouldBe` Right ""

        it "rejects unsupported output and malformed assistant content"
            $ forM_
                [
                    ( object ["type" .= ("function_call" :: Text)]
                    , ProtocolError "unsupported output item; this harness has no tools yet"
                    )
                , (object ["type" .= ("message" :: Text)], ProtocolError "missing assistant message content")
                ,
                    ( object ["type" .= ("message" :: Text), "content" .= [object ["type" .= ("unknown" :: Text)]]]
                    , ProtocolError "unsupported assistant content"
                    )
                ,
                    ( object
                        [ "type" .= ("message" :: Text)
                        , "content" .= [object ["type" .= ("output_text" :: Text), "text" .= (123 :: Int)]]
                        ]
                    , ProtocolError "invalid assistant text"
                    )
                ]
            $ \(item, expected) -> replyText (Reply [item]) `shouldBe` Left expected

itemDone :: Int -> Value -> Value
itemDone index item = object ["type" .= ("response.output_item.done" :: Text), "output_index" .= index, "item" .= item]
