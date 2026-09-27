module Rockstar.Codex (
    withClient,
    fetchReply,
    fetchReplyAt,
    replyText,
    requestBody,
    CodexError (..),
) where

import           Control.Exception                (Exception, catch, throwIO,
                                                   try)
import           Control.Monad                    (unless, when)
import           Control.Monad.IO.Class           (liftIO)
import           Control.Monad.Trans.State.Strict (StateT (..))
import           Data.Aeson
import qualified Data.Aeson.KeyMap                as KeyMap
import           Data.Aeson.Types                 (parseMaybe)
import qualified Data.ByteString                  as BS
import qualified Data.ByteString.Char8            as BS8
import           Data.Char                        (isSpace, toLower)
import           Data.Foldable                    (toList)
import           Data.IORef
import qualified Data.Map.Strict                  as Map
import           Data.Maybe                       (isJust)
import           Data.Text                        (Text)
import qualified Data.Text                        as Text
import           Data.Text.Encoding               (encodeUtf8)
import           Data.Word                        (Word64)
import           Network.HTTP.Client              hiding (requestBody)
import qualified Network.HTTP.Client              as HTTP
import           Network.HTTP.Types               (statusCode)
import qualified Rockstar.Auth                    as Auth
import           Rockstar.Chat.Types
import           Rockstar.Credentials             (Credentials (..),
                                                   CredentialsM)
import qualified Rockstar.Http                    as Http

data CodexError
    = AuthenticationRejected
    | AccessDenied
    | RateLimited
    | RequestRejected Int
    | UnexpectedContentType
    | ProtocolError Text
    | GenerationFailed
    | GenerationIncomplete
    | CodexNetwork Text
    deriving (Eq)
instance Exception CodexError
instance Show CodexError where
    show AuthenticationRejected = "Codex rejected authentication; run `rockstar login` again"
    show AccessDenied = "Codex denied access; check your account's access to this model and Fast mode"
    show RateLimited = "Codex rate or credit limit reached; try again later"
    show (RequestRejected code) =
        "Codex request failed (HTTP "
            <> show code
            <> "); check model, max reasoning, and priority availability"
    show UnexpectedContentType = "Codex did not return a text/event-stream response"
    show (ProtocolError reason) = "Invalid Codex response stream: " <> Text.unpack reason
    show GenerationFailed = "Codex generation failed; the incomplete turn was not saved"
    show GenerationIncomplete = "Codex generation was incomplete; the incomplete turn was not saved"
    show (CodexNetwork operation) = "Network error during " <> Text.unpack operation

withClient :: (Manager -> CredentialsM a) -> CredentialsM a
withClient action = do
    Auth.requireSignedIn
    StateT $ \state -> Http.withManager $ \manager -> runStateT (action manager) state

fetchReply :: Manager -> Conversation -> CredentialsM (Either CodexError Reply)
fetchReply manager conversation = do
    credentials <- Auth.loadCredentials manager
    liftIO $
        try $
            fetchReplyAt "https://chatgpt.com/backend-api/codex/responses" manager credentials conversation

fetchReplyAt :: String -> Manager -> Credentials -> Conversation -> IO Reply
fetchReplyAt url manager auth conversation =
    exchange `catch` \(error' :: Http.HttpError) ->
        throwIO
            ( CodexNetwork $ case error' of
                Http.NetworkFailure operation -> operation
                Http.ResponseTooLarge _       -> "reading a Codex response"
            )
  where
    exchange = Http.network "requesting a Codex response" $ do
        let token = encodeUtf8 (accessToken auth)
            account = encodeUtf8 (accountId auth)
            validHeader bytes = not (BS.null bytes) && BS.all (\byte -> byte >= 32 && byte /= 127) bytes
        unless (validHeader token && validHeader account) $
            throwIO (ProtocolError "invalid authentication header")
        base <- Http.request url
        let request =
                base
                    { method = "POST"
                    , HTTP.requestBody = RequestBodyLBS (encode $ requestBody conversation)
                    , requestHeaders =
                        requestHeaders base
                            <> [ ("Authorization", "Bearer " <> token)
                               , ("ChatGPT-Account-Id", account)
                               , ("originator", "rockstar")
                               , ("OpenAI-Beta", "responses=experimental")
                               , ("Accept", "text/event-stream")
                               , ("Content-Type", "application/json")
                               ]
                    }
        withResponse request manager $ \response -> do
            let code = statusCode (responseStatus response)
            unless (code >= 200 && code < 300) $ throwIO $ case code of
                401 -> AuthenticationRejected
                403 -> AccessDenied
                429 -> RateLimited
                _   -> RequestRejected code
            let contentType =
                    (BS8.map toLower . BS8.dropWhileEnd isSpace . BS8.dropWhile isSpace . BS8.takeWhile (/= ';'))
                        <$> lookup "Content-Type" (responseHeaders response)
            -- Codex can omit Content-Type even when the body is a valid SSE stream.
            unless (maybe True (== "text/event-stream") contentType) $ throwIO UnexpectedContentType
            state <- newIORef (StreamState Map.empty Nothing)
            readSse (responseBody response) $ \bytes -> do
                if BS8.dropWhileEnd isSpace (BS8.dropWhile isSpace bytes) == "[DONE]"
                    then pure True
                    else
                        if BS8.all isSpace bytes
                            then pure False
                            else do
                                value <-
                                    either (const $ throwIO $ ProtocolError "invalid event JSON") pure (eitherDecodeStrict' bytes)
                                previous <- readIORef state
                                next <- either throwIO pure (advance previous value)
                                writeIORef state next
                                pure (isJust $ completedReply next)
            final <- readIORef state
            maybe
                (throwIO $ ProtocolError "stream ended without a completed response")
                pure
                (completedReply final)

requestBody :: Conversation -> Value
requestBody conversation =
    object
        [ "model" .= model conversation
        , "instructions" .= ("You are a helpful assistant." :: Text)
        , "input" .= items conversation
        , "stream" .= True
        , "store" .= False
        , "reasoning" .= object ["effort" .= ("max" :: Text), "summary" .= ("auto" :: Text)]
        , "service_tier" .= ("priority" :: Text)
        , "include" .= (["reasoning.encrypted_content"] :: [Text])
        , "tools" .= ([] :: [Value])
        , "tool_choice" .= ("none" :: Text)
        ]

data StreamState = StreamState
    { completedItems :: Map.Map Word64 Value
    , completedReply :: Maybe Reply
    }

advance :: StreamState -> Value -> Either CodexError StreamState
advance state value = do
    kind <- required "missing event type" (field "type" value >>= textValue)
    case kind of
        "response.output_item.done" -> do
            index <- required "missing output item index" (field "output_index" value >>= parseMaybe parseJSON)
            item <- required "missing completed output item" (field "item" value >>= objectValue)
            pure state{completedItems = Map.insert index item (completedItems state)}
        "response.completed" -> finish
        "response.done" -> finish
        "error" -> Left GenerationFailed
        "response.failed" -> Left GenerationFailed
        "response.incomplete" -> Left GenerationIncomplete
        _ -> pure state
  where
    finish = do
        response <- required "missing completed response" (field "response" value)
        case field "status" response >>= textValue of
            Just "completed" -> pure ()
            Just "failed" -> Left GenerationFailed
            Just "cancelled" -> Left GenerationFailed
            Just "incomplete" -> Left GenerationIncomplete
            _ -> Left (ProtocolError "missing or unexpected terminal status")
        output <- completedOutput response (completedItems state)
        pure state{completedReply = Just (Reply output)}

completedOutput :: Value -> Map.Map Word64 Value -> Either CodexError [Value]
completedOutput response collectedItems = case field "output" response of
    Just (Array output) | not (null output) || Map.null collectedItems -> pure (toList output)
    Nothing | not (Map.null collectedItems) -> fromItems
    Just (Array _) | not (Map.null collectedItems) -> fromItems
    _ -> Left (ProtocolError "missing output in completed response")
  where
    fromItems = do
        unless (Map.keys collectedItems == take (Map.size collectedItems) [0 ..]) $
            Left (ProtocolError "missing completed output items")
        pure (Map.elems collectedItems)

replyText :: Reply -> Either CodexError Text
replyText reply = Text.concat <$> mapM itemText (outputItems reply)
  where
    itemText item = case field "type" item >>= textValue of
        Just "reasoning" -> pure ""
        Just "message" -> do
            content <- required "missing assistant message content" (field "content" item >>= arrayValue)
            Text.concat <$> mapM blockText content
        _ -> Left (ProtocolError "unsupported output item; this harness has no tools yet")
    blockText block = do
        key <- case field "type" block >>= textValue of
            Just "output_text" -> pure "text"
            Just "refusal" -> pure "refusal"
            _ -> Left (ProtocolError "unsupported assistant content")
        required "invalid assistant text" (field key block >>= textValue)

field :: Key -> Value -> Maybe Value
field key (Object object') = KeyMap.lookup key object'
field _ _                  = Nothing

textValue :: Value -> Maybe Text
textValue (String text) = Just text
textValue _             = Nothing

arrayValue :: Value -> Maybe [Value]
arrayValue (Array values) = Just (toList values)
arrayValue _              = Nothing

objectValue :: Value -> Maybe Value
objectValue value@(Object _) = Just value
objectValue _                = Nothing

required :: Text -> Maybe a -> Either CodexError a
required message = maybe (Left $ ProtocolError message) Right

-- Incremental SSE framing: LF, CRLF, CR, comments, multi-line data, and a UTF-8 BOM.
-- Bound both partial lines and complete events before allocating an unbounded buffer.
readSse :: BodyReader -> (BS.ByteString -> IO Bool) -> IO ()
readSse reader emit = readChunk [] 0 [] 0 False True
  where
    limit = 16 * 1024 * 1024
    checkSize size = when (size > limit) $ throwIO (ProtocolError "event too large")
    readChunk lineParts lineSize dataLines dataSize skipLF firstLine = do
        chunk <- brRead reader
        unless (BS.null chunk) $ feed lineParts lineSize dataLines dataSize skipLF firstLine chunk
    feed lineParts lineSize dataLines dataSize skipLF firstLine chunk
        | BS.null chunk = readChunk lineParts lineSize dataLines dataSize skipLF firstLine
        | skipLF && BS.head chunk == 10 =
            feed lineParts lineSize dataLines dataSize False firstLine (BS.tail chunk)
        | otherwise = do
            let (part, rest) = BS.break (\byte -> byte == 10 || byte == 13) chunk
                total = lineSize + BS.length part
            checkSize (total + dataSize)
            if BS.null rest
                then readChunk (part : lineParts) total dataLines dataSize False firstLine
                else do
                    let raw = BS.concat (reverse $ part : lineParts)
                        line = if firstLine then maybe raw id (BS.stripPrefix "\239\187\191" raw) else raw
                        nextSkipLF = BS.head rest == 13
                        continue lines' size' = feed [] 0 lines' size' nextSkipLF False (BS.tail rest)
                    if BS.null line
                        then do
                            stopped <- if null dataLines then pure False else emit (BS.intercalate "\n" $ reverse dataLines)
                            unless stopped $ continue [] 0
                        else case BS.stripPrefix "data:" line of
                            Just rawData -> do
                                let text = maybe rawData id (BS.stripPrefix " " rawData)
                                    size' = dataSize + BS.length text + 1
                                checkSize size'
                                continue (text : dataLines) size'
                            Nothing | line == "data" -> continue ("" : dataLines) (dataSize + 1)
                            _ -> continue dataLines dataSize
