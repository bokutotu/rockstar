module CliSpec (spec) where

import qualified Data.ByteString       as BS
import qualified Data.Text             as Text
import           Data.Text.Encoding    (decodeUtf8, encodeUtf8)
import           Rockstar.Auth.Storage (saveLogin)
import           Rockstar.Auth.Types   (StoredCredentials (..), unixNow)
import           System.Directory      (doesPathExist)
import           System.Exit           (ExitCode (..))
import           System.FilePath       ((</>))
import           System.IO             (hFlush, hPutStr)
import           System.IO.Temp        (withSystemTempDirectory)
import           System.Posix.Signals  (keyboardSignal, signalProcess)
import           System.Process        (getPid, withCreateProcess)
import           Test.Hspec
import           TestSupport

spec :: Spec
spec = describe "CLI" $ do
    it "status/logout do not create storage and missing login is actionable" $ withHome $ \home -> do
        runCli home ["auth", "status"] ""
            `shouldReturn` (ExitSuccess, "Not signed in. Run `rockstar login`.\n", "")
        runCli home ["auth", "logout"] "" `shouldReturn` (ExitSuccess, "Local credentials removed.\n", "")
        doesPathExist (home </> ".rockstar") `shouldReturn` False
        runCli home [] ""
            `shouldReturn` (ExitFailure 1, "", "error: Not signed in. Run `rockstar login` first\n")

    it "shows local status and logs out without displaying tokens" $ withHome $ \home -> do
        putCredentials home False
        runCli home ["auth", "status"] ""
            `shouldReturn` (ExitSuccess, "Signed in (local token is unexpired; server access has not been checked).\n", "")
        runCli home ["auth", "logout"] "" `shouldReturn` (ExitSuccess, "Local credentials removed.\n", "")
        runCli home ["auth", "logout"] "" `shouldReturn` (ExitSuccess, "Local credentials removed.\n", "")
        runCli home ["auth", "status"] ""
            `shouldReturn` (ExitSuccess, "Not signed in. Run `rockstar login`.\n", "")
        doesPathExist (home </> ".rockstar" </> "auth.json") `shouldReturn` False

    it "reports expiration locally without refreshing" $ withHome $ \home -> do
        putCredentials home True
        runCli home ["auth", "status"] ""
            `shouldReturn` ( ExitSuccess
                           , "Signed in; token expired or expires soon. It will refresh before the next request.\n"
                           , ""
                           )
        runCli home [] "/exit\n" `shouldReturn` (ExitSuccess, banner "gpt-6-astra" <> "> ", "")

    it "slash-exit does not wait for stdin to close" $ withHome $ \home -> do
        putCredentials home False
        command <- cliProcess home []
        withCreateProcess command $ \inputPipe outputPipe errorPipe process -> do
            Just input <- pure inputPipe
            Just output <- pure outputPipe
            Just errors <- pure errorPipe
            hPutStr input "/exit\n"
            hFlush input
            waitOutput process output errors `shouldReturn` (ExitSuccess, banner "gpt-6-astra" <> "> ", "")

    it "EOF and model overrides exit without sending requests" $ withHome $ \home -> do
        putCredentials home False
        runCli home [] "" `shouldReturn` (ExitSuccess, banner "gpt-6-astra" <> "> \n", "")
        runCli home ["--model", "another-model"] "/exit\n"
            `shouldReturn` (ExitSuccess, banner "another-model" <> "> ", "")

    it "Ctrl+C interrupts input without leaving a blocked stdin thread" $ withHome $ \home -> do
        putCredentials home False
        command <- cliProcess home []
        withCreateProcess command $ \_ outputPipe errorPipe process -> do
            Just output <- pure outputPipe
            Just errors <- pure errorPipe
            let expectedPrefix = banner "gpt-6-astra" <> "> "
                prefixBytes = encodeUtf8 (Text.pack expectedPrefix)
            prefix <- within $ BS.hGet output (BS.length prefixBytes)
            pid <- getPid process >>= maybe (fail "child has no PID") pure
            signalProcess keyboardSignal pid
            (code, remaining, stderr') <- waitOutput process output errors
            (code, Text.unpack (decodeUtf8 prefix) <> remaining, stderr')
                `shouldBe` (ExitSuccess, expectedPrefix <> "\n", "")

    it "reports corrupt credentials without revealing their contents" $ withHome $ \home -> do
        putCredentials home False
        BS.writeFile (home </> ".rockstar" </> "auth.json") "{secret-do-not-print"
        runCli home ["auth", "status"] ""
            `shouldReturn` (ExitFailure 1, "", "error: Invalid auth.json; run `rockstar login` to replace it\n")

    it "supports help/version without creating files" $ withHome $ \home -> do
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
                    , "  auth                     Inspect or remove locally stored credentials"
                    , "  help                     Print command help"
                    , ""
                    , "Run without a subcommand to chat. Type /exit or send EOF to quit. Defaults:"
                    , "gpt-6-astra, reasoning=max, Fast mode on (priority)."
                    ]
        runCli home ["--help"] "" `shouldReturn` (ExitSuccess, help, "")
        runCli home ["help"] "" `shouldReturn` (ExitSuccess, help, "")
        runCli home ["--version"] "" `shouldReturn` (ExitSuccess, "rockstar 0.1.0\n", "")
        doesPathExist (home </> ".rockstar") `shouldReturn` False

withHome :: (FilePath -> IO a) -> IO a
withHome = withSystemTempDirectory "rockstar-cli"

putCredentials :: FilePath -> Bool -> IO ()
putCredentials home expired = do
    now <- unixNow
    saveLogin
        (home </> ".rockstar")
        ( StoredCredentials
            "fake-access-do-not-print"
            "fake-refresh-do-not-print"
            (if expired then 1 else now + 3600)
            "test-account"
        )

banner :: String -> String
banner model =
    "rockstar · "
        <> model
        <> " · reasoning max · fast on\n"
        <> "Type /exit to quit. Conversation history is not saved.\n\n"
