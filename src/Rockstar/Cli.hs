module Rockstar.Cli (main) where

import           Control.Exception
import           Control.Monad.IO.Class           (liftIO)
import           Control.Monad.Trans.State.Strict (evalStateT)
import qualified Data.Text                        as Text
import           Options.Applicative
import qualified Rockstar.Chat                    as Chat
import           Rockstar.Chat.Types              (defaultModel)
import           Rockstar.Credentials             (CredentialsM)
import           Rockstar.Interrupt               (Interrupted)
import qualified Rockstar.Login                   as Login
import           System.Exit
import           System.IO
import           System.IO.Error                  (ioeGetErrorString,
                                                   isResourceVanishedError,
                                                   isUserError)

data Command = Login | Help [String]
data Cli = Cli Text.Text (Maybe Command)

main :: IO ()
main = do
    mapM_ (`hSetEncoding` utf8) [stdin, stdout, stderr]
    options <- execParser parserInfo
    evalStateT (run options) Nothing `catch` reportError

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
                        <> command
                            "help"
                            (info (Help <$> many (strArgument (metavar "COMMAND"))) (progDesc "Print command help"))
                    )
                )

run :: Cli -> CredentialsM ()
run (Cli selectedModel command') = case command' of
    Nothing -> Chat.runChat selectedModel
    Just Login -> Login.login
    Just (Help commands) ->
        liftIO (handleParseResult (execParserPure defaultPrefs parserInfo (commands <> ["--help"]))) >>= run

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
    message
        | Just error' <- fromException exception, isUserError error' = ioeGetErrorString error'
        | otherwise = case exception of SomeException error' -> displayException error'
    interrupted = hPutStrLn stderr message >> exitWith (ExitFailure 130)
