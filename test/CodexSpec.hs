module CodexSpec (spec) where

import           Control.Exception                (try)
import           Control.Monad                    (forM_)
import           Control.Monad.Trans.State.Strict (runStateT)
import           Data.Aeson
import qualified Data.ByteString                  as BS
import qualified Data.ByteString.Builder          as Builder
import qualified Data.ByteString.Char8            as BS8
import qualified Data.ByteString.Lazy             as LBS
import           Data.IORef
import           Data.List                        (sort)
import           Data.Text                        (Text)
import qualified Data.Text                        as Text
import           Data.Text.Encoding               (encodeUtf8)
import           Data.Time.Clock.POSIX            (getPOSIXTime)
import           Network.HTTP.Types
import           Network.Wai
import           Rockstar.Chat.Types
import           Rockstar.Codex
import           Rockstar.Credentials             (Credentials (..))
import qualified Rockstar.Http                    as Http
import           System.FilePath                  ((</>))
import           Test.Hspec
import           TestSupport

spec :: Spec
spec = describe "Codex transport" $ do
    it "sends complete headers and settings, preserves reasoning, and decodes split UTF-8" $ Http.withManager $ \manager -> do
        -- Arrange
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
            auth = credentials 4600
            userHello =
                object
                    [ "type" .= ("message" :: Text)
                    , "role" .= ("user" :: Text)
                    , "content" .= [object ["type" .= ("input_text" :: Text), "text" .= ("hello" :: Text)]]
                    ]
            userContinue =
                object
                    [ "type" .= ("message" :: Text)
                    , "role" .= ("user" :: Text)
                    , "content" .= [object ["type" .= ("input_text" :: Text), "text" .= ("continue" :: Text)]]
                    ]
            conversation = Conversation defaultModel [userHello]
            next = Conversation defaultModel [userHello, reasoning, message "こんにちは", userContinue]
            firstBody =
                object
                    [ "model" .= ("gpt-6-astra" :: Text)
                    , "instructions" .= ("You are a helpful assistant." :: Text)
                    , "input" .= [userHello]
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
                    , "input" .= [userHello, reasoning, message "こんにちは", userContinue]
                    , "stream" .= True
                    , "store" .= False
                    , "reasoning" .= object ["effort" .= ("max" :: Text), "summary" .= ("auto" :: Text)]
                    , "service_tier" .= ("priority" :: Text)
                    , "include" .= (["reasoning.encrypted_content"] :: [Text])
                    , "tools" .= ([] :: [Value])
                    , "tool_choice" .= ("none" :: Text)
                    ]
        withServer app $ \url -> do
            let expectedHeaders payload =
                    sort
                        [ ("Accept", "text/event-stream")
                        , ("Accept-Encoding", "gzip")
                        , ("Authorization", "Bearer " <> encodeUtf8 (accessTokenFor 4600))
                        , ("ChatGPT-Account-Id", "test-account")
                        , ("Content-Type", "application/json")
                        , ("Content-Length", BS8.pack $ show $ LBS.length $ encode payload)
                        , ("Host", BS8.pack $ drop 7 url)
                        , ("OpenAI-Beta", "responses=experimental")
                        , ("originator", "rockstar")
                        , ("User-Agent", "rockstar/0.1.0")
                        ]
                expected =
                    ( Reply output
                    , Reply output
                    , [(expectedHeaders firstBody, firstBody), (expectedHeaders secondBody, secondBody)]
                    )

            -- Act
            let action = do
                    first <- fetchReplyAt url manager auth conversation
                    second <- fetchReplyAt url manager auth next
                    requests <- readIORef received
                    pure (first, second, requests)

            -- Assert
            action `shouldReturn` expected

    it "keeps refreshed credentials across failed requests" $ withHome $ \home -> do
        -- Arrange
        now <- floor <$> getPOSIXTime
        received <- newIORef []
        let expiry = now + 3600
            token = accessTokenFor expiry
            conversation = Conversation defaultModel []
            expectedCredentials =
                object
                    [ "accessToken" .= token
                    , "refreshToken" .= ("rotated-refresh" :: Text)
                    , "expiresAt" .= expiry
                    , "accountId" .= ("test-account" :: Text)
                    ]
            expected =
                ( Left RateLimited
                , Right $ Reply [message "hello"]
                , Just expectedCredentials
                , Just expectedCredentials
                ,
                    [ ("/oauth/token", Nothing)
                    , ("/backend-api/codex/responses", Just $ "Bearer " <> encodeUtf8 token)
                    , ("/backend-api/codex/responses", Just $ "Bearer " <> encodeUtf8 token)
                    ]
                )
            app request respond = do
                previous <- readIORef received
                modifyIORef' received (<> [(rawPathInfo request, lookup "Authorization" $ requestHeaders request)])
                if pathInfo request == ["oauth", "token"]
                    then
                        respond $
                            responseLBS status200 [] $
                                encode $
                                    object
                                        [ "access_token" .= token
                                        , "refresh_token" .= ("rotated-refresh" :: Text)
                                        , "expires_in" .= (3600 :: Int)
                                        ]
                    else
                        if length previous == 1
                            then respond $ responseLBS status429 [] "rate limited"
                            else
                                respond $
                                    responseLBS status200 [("Content-Type", "text/event-stream")] $
                                        sse [completeEvent [message "hello"]]

        -- Act
        let action = withServer app $ \url -> do
                manager <- managerAt url
                ((first, second), cached) <-
                    runStateT
                        ( do
                            first <- fetchReply manager conversation
                            second <- fetchReply manager conversation
                            pure (first, second)
                        )
                        (Just $ credentials 1)
                stored <- decodeStrict' <$> BS.readFile (home </> ".rockstar" </> "auth.json")
                requests <- readIORef received
                pure (first, second, toJSON <$> cached, stored, requests)

        -- Assert
        action `shouldReturn` expected

    it "accepts headerless SSE and collects ordered output when completed output is empty" $ Http.withManager $ \manager -> do
        -- Arrange
        let reasoning = object ["type" .= ("reasoning" :: Text), "encrypted_content" .= ("opaque-reasoning" :: Text)]
            body = sse [itemDone 1 (message "こんにちは"), itemDone 0 reasoning, completeEvent []]
            app _ respond = respond (responseLBS status200 [] body)
            expected = Reply [reasoning, message "こんにちは"]

        -- Act
        let action = withServer app $ \url -> fetchReplyAt url manager (credentials 4600) (Conversation defaultModel [])

        -- Assert
        action `shouldReturn` expected

    it "supports output-item fallback and response.done" $ Http.withManager $ \manager -> do
        -- Arrange
        let body =
                sse
                    [ itemDone 0 (message "hello")
                    , object
                        ["type" .= ("response.done" :: Text), "response" .= object ["status" .= ("completed" :: Text)]]
                    ]
            expected = Reply [message "hello"]

        -- Act
        let action = withServer (streamApp body) $ \url -> fetchReplyAt url manager (credentials 4600) (Conversation defaultModel [])

        -- Assert
        action `shouldReturn` expected

    it "ignores text and refusal deltas and returns only the completed output" $ Http.withManager $ \manager -> do
        -- Arrange
        let body =
                sse
                    [ object ["type" .= ("response.output_text.delta" :: Text), "delta" .= ("partial text" :: Text)]
                    , object ["type" .= ("response.refusal.delta" :: Text), "delta" .= ("partial refusal" :: Text)]
                    , completeEvent [message "final answer"]
                    ]
            expected = Reply [message "final answer"]

        -- Act
        let action = withServer (streamApp body) $ \url -> fetchReplyAt url manager (credentials 4600) (Conversation defaultModel [])

        -- Assert
        action `shouldReturn` expected

    it "frames comments, BOM, multi-line data, and all SSE newline styles" $ Http.withManager $ \manager ->
        forM_ ["\n", "\r", "\r\n"] $ \newline -> do
            -- Arrange
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
                expected = Reply []

            -- Act
            let action = withServer app $ \url -> fetchReplyAt url manager (credentials 4600) (Conversation defaultModel [])

            -- Assert
            action `shouldReturn` expected

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
                         , error'
                         )
                       | (status', error') <-
                            [ ("failed" :: Text, GenerationFailed)
                            , ("cancelled", GenerationFailed)
                            , ("incomplete", GenerationIncomplete)
                            , ("unknown", ProtocolError "missing or unexpected terminal status")
                            ]
                       ]
        forM_ cases $ \(body, error') ->
            forM_ [[], [("Content-Type", "text/event-stream")]] $ \headers -> do
                -- Arrange
                let app _ respond = respond (responseLBS status200 headers body)
                    expected = Left error'

                -- Act
                let action = withServer app $ \url -> try (fetchReplyAt url manager (credentials 4600) (Conversation defaultModel []))

                -- Assert
                action `shouldReturn` expected

    it "bounds partial events before a newline arrives" $ Http.withManager $ \manager -> do
        -- Arrange
        let body = "data: " <> LBS.fromStrict (BS.replicate (16 * 1024 * 1024) 120)
            expected = Left (ProtocolError "event too large")

        -- Act
        let action = withServer (streamApp body) $ \url -> try (fetchReplyAt url manager (credentials 4600) (Conversation defaultModel []))

        -- Assert
        action `shouldReturn` expected

    it "rejects HTTP errors without retrying or printing response bodies" $ Http.withManager $ \manager ->
        forM_
            [ (status401, AuthenticationRejected)
            , (status403, AccessDenied)
            , (status429, RateLimited)
            , (status502, RequestRejected 502)
            ] $ \(status', error') -> do
            -- Arrange
            calls <- newIORef (0 :: Int)
            let app _ respond = modifyIORef' calls (+ 1) >> respond (responseLBS status' [] "secret diagnostics")
                expected = (Left error', 1)

            -- Act
            let action = withServer app $ \url -> do
                    result <- try (fetchReplyAt url manager (credentials 4600) (Conversation defaultModel []))
                    count <- readIORef calls
                    pure (result, count)

            -- Assert
            action `shouldReturn` expected

    it "rejects explicitly non-SSE Content-Types" $ Http.withManager $ \manager ->
        forM_ ["application/json", "text/html", ""] $ \contentType -> do
            -- Arrange
            let app _ respond = respond (responseLBS status200 [("Content-Type", contentType)] (sse [completeEvent []]))
                expected = Left UnexpectedContentType

            -- Act
            let action = withServer app $ \url -> try (fetchReplyAt url manager (credentials 4600) (Conversation defaultModel []))

            -- Assert
            action `shouldReturn` expected

    it "prevents authentication-header injection before sending a request" $ Http.withManager $ \manager -> do
        -- Arrange
        calls <- newIORef (0 :: Int)
        let input = Credentials "bad\r\nInjected: yes" "refresh" 4600 "test-account"
            app _ respond = modifyIORef' calls (+ 1) >> respond (responseLBS status200 [] "")
            expected = (Left (ProtocolError "invalid authentication header"), 0)

        -- Act
        let action = withServer app $ \url -> do
                result <- try (fetchReplyAt url manager input (Conversation defaultModel []))
                count <- readIORef calls
                pure (result, count)

        -- Assert
        action `shouldReturn` expected

    it "never forwards access tokens through redirects" $ Http.withManager $ \manager -> do
        -- Arrange
        calls <- newIORef (0 :: Int)
        let destination _ respond = modifyIORef' calls (+ 1) >> respond (responseLBS status200 [] "")
            expected = (Left (RequestRejected 307), 0)

        -- Act
        let action = withServer destination $ \target -> do
                let origin _ respond = respond (responseLBS status307 [("Location", encodeUtf8 $ Text.pack target)] "")
                result <- withServer origin $ \url -> try (fetchReplyAt url manager (credentials 4600) (Conversation defaultModel []))
                count <- readIORef calls
                pure (result, count)

        -- Assert
        action `shouldReturn` expected

    describe "replyText" $ do
        it "extracts text and refusals, not reasoning" $ do
            -- Arrange
            let reasoning = object ["type" .= ("reasoning" :: Text), "encrypted_content" .= ("opaque-reasoning" :: Text)]
                refusal =
                    object
                        [ "type" .= ("message" :: Text)
                        , "content" .= [object ["type" .= ("refusal" :: Text), "refusal" .= ("no" :: Text)]]
                        ]
                inputs = [Reply [reasoning, message "hello\n", refusal], Reply []]
                expected = [Right "hello\nno", Right ""]

            -- Act
            let actual = map replyText inputs

            -- Assert
            actual `shouldBe` expected

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
            $ \(item, error') -> do
                -- Arrange
                let input = Reply [item]
                    expected = Left error'

                -- Act
                let actual = replyText input

                -- Assert
                actual `shouldBe` expected

itemDone :: Int -> Value -> Value
itemDone index item = object ["type" .= ("response.output_item.done" :: Text), "output_index" .= index, "item" .= item]
