module Rockstar.Auth (login, status) where

import           Control.Concurrent    (forkIO)
import           Control.Exception     (AsyncException (UserInterrupt),
                                        IOException, catch, throwIO)
import           Control.Monad         (void, when)
import qualified Data.Text             as Text
import qualified Data.Text.IO          as Text
import           Rockstar.Auth.OAuth
import           Rockstar.Auth.Storage
import           Rockstar.Auth.Types
import qualified Rockstar.Http         as Http
import           Rockstar.Interrupt
import           System.Exit           (ExitCode (ExitSuccess))
import           System.FilePath       ((</>))
import           System.Info           (os)
import           System.IO             (hFlush, stderr, stdout)
import           System.Process

login :: IO ()
login =
    signIn `catch` \interruption -> case interruption of
        UserInterrupt -> throwIO (Interrupted "Login cancelled")
        other         -> throwIO other
  where
    signIn = Http.withManager $ \manager -> do
        directory <- credentialDirectory
        attempt <- beginLogin
        withCallback attempt 1455 $ \_ waitForCode -> do
            let url = authorizationUrl attempt
            Text.putStrLn
                ( "Open this URL to sign in:\n"
                    <> url
                    <> "\n\nWaiting for authorization (10-minute timeout; Ctrl+C cancels)..."
                )
            hFlush stdout
            openBrowser (Text.unpack url)
            code <- waitForCode
            finishOnUserInterrupt $ do
                credentials <- exchangeCode manager attempt code
                saveLogin directory credentials
            putStrLn ("Signed in. Credentials saved to " <> (directory </> "auth.json") <> ".")

status :: IO ()
status = do
    directory <- credentialDirectory
    credentials <- readCredentials directory
    now <- unixNow
    putStrLn $ case authStatusAt now credentials of
        SignedOut -> "Not signed in. Run `rockstar login`."
        TokenUnexpired -> "Signed in (local token is unexpired; server access has not been checked)."
        RefreshRequired -> "Signed in; token expired or expires soon. It will refresh before the next request."

openBrowser :: String -> IO ()
openBrowser url = launch `catch` \(_ :: IOException) -> manual
  where
    manual = Text.hPutStrLn stderr "Could not open a browser automatically. Open the URL above manually."
    launch = do
        let command = if os == "darwin" then "open" else "xdg-open"
        (_, _, _, process) <-
            createProcess
                (proc command [url])
                    { std_in = NoStream
                    , std_out = NoStream
                    , std_err = NoStream
                    , close_fds = True
                    }
        void $ forkIO $ do
            result <- waitForProcess process
            when (result /= ExitSuccess) manual
