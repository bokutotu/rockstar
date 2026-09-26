module TestSupport (
    credentials,
    accessTokenFor,
    message,
    completeEvent,
    sse,
    withServer,
    streamApp,
    withCredentialDirectory,
    cliProcess,
    runCli,
    within,
    waitOutput,
    testEnvironment,
) where

import           Control.Concurrent.Async   (concurrently)
import           Control.Exception          (evaluate)
import           Data.Aeson
import qualified Data.ByteString.Base64.URL as Base64
import qualified Data.ByteString.Lazy       as LBS
import           Data.Text                  (Text)
import           Data.Text.Encoding         (decodeUtf8)
import           Data.Word                  (Word64)
import           Network.HTTP.Types         (status200)
import           Network.Wai                (Application, responseLBS)
import           Network.Wai.Handler.Warp   (testWithApplication)
import           Rockstar.Auth.Types
import           System.Environment         (getEnvironment)
import           System.Exit                (ExitCode)
import           System.FilePath            ((</>))
import           System.IO                  (Handle, hGetContents)
import           System.IO.Temp             (withSystemTempDirectory)
import           System.Process
import           System.Timeout             (timeout)

accessTokenFor :: Word64 -> Text
accessTokenFor expiry =
    "test-header."
        <> decodeUtf8
            ( Base64.encodeUnpadded $
                LBS.toStrict $
                    encode $
                        object
                            [ "https://api.openai.com/auth" .= object ["chatgpt_account_id" .= ("test-account" :: Text)]
                            , "exp" .= expiry
                            ]
            )
        <> ".test-signature"

credentials :: Word64 -> StoredCredentials
credentials expiry = StoredCredentials (accessTokenFor expiry) "test-refresh-token" expiry "test-account"

message :: Text -> Value
message text =
    object
        [ "type" .= ("message" :: Text)
        , "id" .= ("msg_test" :: Text)
        , "role" .= ("assistant" :: Text)
        , "status" .= ("completed" :: Text)
        , "content"
            .= [ object
                    ["type" .= ("output_text" :: Text), "text" .= text, "annotations" .= ([] :: [Value])]
               ]
        ]

completeEvent :: [Value] -> Value
completeEvent output =
    object
        [ "type" .= ("response.completed" :: Text)
        , "response" .= object ["status" .= ("completed" :: Text), "output" .= output]
        ]

sse :: [Value] -> LBS.ByteString
sse = mconcat . map (\event -> "data: " <> encode event <> "\r\n\r\n")

withServer :: Application -> (String -> IO a) -> IO a
withServer app action = testWithApplication (pure app) $ \port -> action ("http://127.0.0.1:" <> show port)

streamApp :: LBS.ByteString -> Application
streamApp body _ respond = respond (responseLBS status200 [("Content-Type", "text/event-stream; charset=utf-8")] body)

withCredentialDirectory :: (FilePath -> IO a) -> IO a
withCredentialDirectory action = withSystemTempDirectory "rockstar-test" $ \home -> action (home </> ".rockstar")

-- Outbound provider traffic must fail, even if a test accidentally requests it.
testEnvironment :: [(String, String)]
testEnvironment =
    [ ("HTTP_PROXY", "http://127.0.0.1:9")
    , ("HTTPS_PROXY", "http://127.0.0.1:9")
    , ("ALL_PROXY", "http://127.0.0.1:9")
    , ("NO_PROXY", "127.0.0.1,localhost")
    , ("http_proxy", "http://127.0.0.1:9")
    , ("https_proxy", "http://127.0.0.1:9")
    , ("all_proxy", "http://127.0.0.1:9")
    , ("no_proxy", "127.0.0.1,localhost")
    ]

cliProcess :: FilePath -> [String] -> IO CreateProcess
cliProcess home args = do
    inherited <- getEnvironment
    let overrides = ("HOME", home) : testEnvironment
    pure
        (proc "rockstar" args)
            { env = Just (overrides <> filter ((`notElem` map fst overrides) . fst) inherited)
            , std_in = CreatePipe
            , std_out = CreatePipe
            , std_err = CreatePipe
            }

runCli :: FilePath -> [String] -> String -> IO (ExitCode, String, String)
runCli home args input = do
    command <- cliProcess home args
    within $ readCreateProcessWithExitCode command input

within :: IO a -> IO a
within action = timeout (8 * 1000000) action >>= maybe (fail "test timed out") pure

waitOutput :: ProcessHandle -> Handle -> Handle -> IO (ExitCode, String, String)
waitOutput process out err = within $ do
    let readFully handle = do
            content <- hGetContents handle
            _ <- evaluate (length content)
            pure content
    ((stdout', stderr'), code) <-
        concurrently
            (concurrently (readFully out) (readFully err))
            (waitForProcess process)
    pure (code, stdout', stderr')
