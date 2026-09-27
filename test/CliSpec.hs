module CliSpec (spec) where

import           Control.Monad                    (forM_)
import           Control.Monad.Trans.State.Strict (evalStateT)
import qualified Data.ByteString                  as BS
import qualified Data.Text                        as Text
import           Data.Text.Encoding               (decodeUtf8, encodeUtf8)
import           Data.Time.Clock.POSIX            (getPOSIXTime)
import           Rockstar.Credentials             (Credentials (..), save)
import           System.Directory                 (doesPathExist)
import           System.Exit                      (ExitCode (..))
import           System.FilePath                  ((</>))
import           System.IO                        (hFlush, hPutStr)
import           System.Posix.Signals             (keyboardSignal,
                                                   signalProcess)
import           System.Process                   (getPid, withCreateProcess)
import           Test.Hspec
import           TestSupport

spec :: Spec
spec = describe "CLI" $ do
    it "reports missing login without creating storage" $ withHome $ \home -> do
        -- Arrange
        let arguments = []
            input = ""
            expected = ((ExitFailure 1, "", "error: Not signed in. Run `rockstar login` first\n"), False)

        -- Act
        let action = do
                result <- runCli home arguments input
                exists <- doesPathExist (home </> ".rockstar")
                pure (result, exists)

        -- Assert
        action `shouldReturn` expected

    it "rejects the removed auth command without creating storage" $ withHome $ \home -> do
        -- Arrange
        let arguments = ["auth", "status"]
            input = ""
            expected =
                (
                    ( ExitFailure 1
                    , ""
                    , unlines
                        [ "Invalid argument `auth'"
                        , ""
                        , "Usage: rockstar [--model MODEL] [COMMAND] [-V|--version]"
                        , ""
                        , "  An independent Codex chat harness"
                        ]
                    )
                , False
                )

        -- Act
        let action = do
                result <- runCli home arguments input
                exists <- doesPathExist (home </> ".rockstar")
                pure (result, exists)

        -- Assert
        action `shouldReturn` expected

    it "does not refresh expired credentials until a request is sent" $ withHome $ \home -> do
        -- Arrange
        putCredentials True
        let arguments = []
            input = "/exit\n"
            expected = (ExitSuccess, banner "gpt-6-astra" <> "> ", "")

        -- Act
        let action = runCli home arguments input

        -- Assert
        action `shouldReturn` expected

    it "slash-exit does not wait for stdin to close" $ withHome $ \home -> do
        -- Arrange
        putCredentials False
        command <- cliProcess home []
        let expected = (ExitSuccess, banner "gpt-6-astra" <> "> ", "")

        -- Act
        let action = withCreateProcess command $ \inputPipe outputPipe errorPipe process -> do
                Just input <- pure inputPipe
                Just output <- pure outputPipe
                Just errors <- pure errorPipe
                hPutStr input "/exit\n"
                hFlush input
                waitOutput process output errors

        -- Assert
        action `shouldReturn` expected

    it "EOF and model overrides exit without sending requests" $ withHome $ \home -> do
        -- Arrange
        putCredentials False
        let expected =
                [ (ExitSuccess, banner "gpt-6-astra" <> "> \n", "")
                , (ExitSuccess, banner "another-model" <> "> ", "")
                ]

        -- Act
        let action = sequence [runCli home [] "", runCli home ["--model", "another-model"] "/exit\n"]

        -- Assert
        action `shouldReturn` expected

    it "Ctrl+C interrupts input without leaving a blocked stdin thread" $ withHome $ \home -> do
        -- Arrange
        putCredentials False
        command <- cliProcess home []
        let expectedPrefix = banner "gpt-6-astra" <> "> "
            prefixBytes = encodeUtf8 (Text.pack expectedPrefix)
            expected = (ExitSuccess, expectedPrefix <> "\n", "")

        -- Act
        let action = withCreateProcess command $ \_ outputPipe errorPipe process -> do
                Just output <- pure outputPipe
                Just errors <- pure errorPipe
                prefix <- within $ BS.hGet output (BS.length prefixBytes)
                pid <- getPid process >>= maybe (fail "child has no PID") pure
                signalProcess keyboardSignal pid
                (code, remaining, stderr') <- waitOutput process output errors
                pure (code, Text.unpack (decodeUtf8 prefix) <> remaining, stderr')

        -- Assert
        action `shouldReturn` expected

    it "reports corrupt credentials without revealing their contents" $ withHome $ \home -> do
        -- Arrange
        putCredentials False
        BS.writeFile (home </> ".rockstar" </> "auth.json") "{secret-do-not-print"
        let arguments = []
            input = ""
            expected = (ExitFailure 1, "", "error: Invalid auth.json\n")

        -- Act
        let action = runCli home arguments input

        -- Assert
        action `shouldReturn` expected

    describe "help and version" $ do
        let help =
                unlines
                    [ "Usage: rockstar [--model MODEL] [COMMAND] [-V|--version]"
                    , ""
                    , "  An independent Codex chat harness"
                    , ""
                    , "Available options:"
                    , "  --model MODEL            Chat model (must support max reasoning and priority"
                    , "                           processing) (default: gpt-6-astra)"
                    , "  -h,--help                Show this help text"
                    , "  -V,--version             Print version"
                    , ""
                    , "Available commands:"
                    , "  login                    Sign in to Codex through your browser"
                    , "  help                     Print command help"
                    , ""
                    , "Run without a subcommand to chat. Type /exit or send EOF to quit. Defaults:"
                    , "gpt-6-astra, reasoning=max, Fast mode on (priority)."
                    ]
        forM_ [(["--help"], help), (["help"], help), (["--version"], "rockstar 0.1.0\n")] $ \(arguments, output) ->
            it ("supports " <> unwords arguments <> " without creating files") $ withHome $ \home -> do
                -- Arrange
                let input = ""
                    expected = ((ExitSuccess, output, ""), False)

                -- Act
                let action = do
                        result <- runCli home arguments input
                        exists <- doesPathExist (home </> ".rockstar")
                        pure (result, exists)

                -- Assert
                action `shouldReturn` expected

putCredentials :: Bool -> IO ()
putCredentials expired = do
    now <- floor <$> getPOSIXTime
    evalStateT
        ( save $
            Credentials
                "fake-access-do-not-print"
                "fake-refresh-do-not-print"
                (if expired then 1 else now + 3600)
                "test-account"
        )
        Nothing

banner :: String -> String
banner model =
    "rockstar · "
        <> model
        <> " · reasoning max · fast on\nType /exit to quit. Conversation history is not saved.\n\n"
