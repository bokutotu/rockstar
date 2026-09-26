module Rockstar.Cli (main) where

import           Control.Exception
import qualified Data.Text           as Text
import           Options.Applicative
import qualified Rockstar.Auth       as Auth
import qualified Rockstar.Chat       as Chat
import           Rockstar.Chat.Types (defaultModel)
import           Rockstar.Interrupt  (Interrupted)
import           System.Exit
import           System.IO
import           System.IO.Error     (isResourceVanishedError)

data Command = Login | AuthStatus | AuthLogout | Help [String]
data Cli = Cli Text.Text (Maybe Command)

main :: IO ()
main = do
    mapM_ (`hSetEncoding` utf8) [stdin, stdout, stderr]
    options <- execParser parserInfo
    run options `catch` reportError

parserInfo :: ParserInfo Cli
parserInfo =
    info
        (parser <**> helper <**> version)
        ( fullDesc
            <> progDesc "An independent Codex chat harness"
            <> footer
                "Run without a subcommand to chat. Type /exit or send EOF to quit.\nDefaults: gpt-6-astra, reasoning=max, Fast mode on (priority)."
        )
  where
    version = infoOption "rockstar 0.1.0" (long "version" <> short 'V' <> help "Print version")
    parser =
        Cli
            <$> strOption
                ( long "model"
                    <> metavar "MODEL"
                    <> value defaultModel
                    <> showDefaultWith Text.unpack
                    <> help "Chat model (must support max reasoning and priority processing)"
                )
            <*> optional
                ( hsubparser
                    ( command "login" (info (pure Login) (progDesc "Sign in to Codex through your browser"))
                        <> command "auth" (info authCommands (progDesc "Inspect or remove locally stored credentials"))
                        <> command
                            "help"
                            (info (Help <$> many (strArgument (metavar "COMMAND"))) (progDesc "Print command help"))
                    )
                )
    authCommands =
        hsubparser
            ( command
                "status"
                (info (pure AuthStatus) (progDesc "Show local authentication status without network requests"))
                <> command
                    "logout"
                    (info (pure AuthLogout) (progDesc "Remove local credentials (does not revoke other sessions)"))
            )

run :: Cli -> IO ()
run (Cli selectedModel command') = case command' of
    Nothing -> Chat.runChat selectedModel
    Just Login -> Auth.login
    Just AuthStatus -> Auth.status
    Just AuthLogout -> Auth.logout
    Just (Help commands) -> handleParseResult (execParserPure defaultPrefs parserInfo (commands <> ["--help"])) >>= run

reportError :: SomeException -> IO ()
reportError exception
    | Just code <- fromException exception = exitWith (code :: ExitCode)
    | Just (_ :: Interrupted) <- fromException exception = interrupted
    | Just UserInterrupt <- fromException exception = interrupted
    | Just ioError' <- fromException exception, isResourceVanishedError ioError' = exitSuccess
    | otherwise = hPutStrLn stderr ("error: " <> message) >> exitFailure
  where
    -- GHC's SomeException display also includes backtraces; the CLI needs only
    -- the underlying exception's safe, user-facing diagnostic.
    message = case exception of SomeException error' -> displayException error'
    interrupted = hPutStrLn stderr message >> exitWith (ExitFailure 130)
