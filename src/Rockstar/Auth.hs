module Rockstar.Auth (login, requireSignedIn, loadCredentials) where

import           Control.Concurrent     (forkIO)
import           Control.Exception      (AsyncException (UserInterrupt),
                                         IOException, catch, throwIO)
import           Control.Monad          (void, when)
import           Control.Monad.IO.Class (liftIO)
import           Data.Maybe             (isNothing)
import qualified Data.Text              as Text
import qualified Data.Text.IO           as Text
import           Data.Time.Clock.POSIX  (getPOSIXTime)
import           Network.HTTP.Client    (Manager)
import           Rockstar.Auth.OAuth
import           Rockstar.Credentials   (Credentials (..), CredentialsM)
import qualified Rockstar.Credentials   as Credentials
import qualified Rockstar.Http          as Http
import           Rockstar.Interrupt
import           System.Exit            (ExitCode (ExitSuccess))
import           System.Info            (os)
import           System.IO              (hFlush, stderr, stdout)
import           System.Process

login :: CredentialsM ()
login = do
    credentials <-
        liftIO $
            signIn `catch` \interruption -> case interruption of
                UserInterrupt -> throwIO (Interrupted "Login cancelled")
                other         -> throwIO other
    Credentials.save credentials
    liftIO $ putStrLn "Signed in."
  where
    signIn = Http.withManager $ \manager -> do
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
            exchangeCode manager attempt code

requireSignedIn :: CredentialsM ()
requireSignedIn = do
    credentials <- Credentials.load
    when (isNothing credentials) $ fail "Not signed in. Run `rockstar login` first"

loadCredentials :: Manager -> CredentialsM Credentials
loadCredentials manager = do
    stored <- Credentials.load >>= maybe (fail "Not signed in. Run `rockstar login` first") pure
    now <- liftIO getPOSIXTime
    if toInteger (expiresAt stored) > floor now + 60
        then pure stored
        else do
            updated <- liftIO $ refreshCredentials manager stored
            Credentials.save updated
            pure updated

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
