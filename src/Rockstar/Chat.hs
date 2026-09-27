module Rockstar.Chat (runChat) where

import           Control.Exception                (AsyncException (UserInterrupt),
                                                   catch, throwIO)
import           Control.Monad.IO.Class           (liftIO)
import           Control.Monad.Trans.State.Strict (mapStateT)
import           Data.Text                        (Text)
import qualified Data.Text                        as Text
import qualified Data.Text.IO                     as Text
import           Rockstar.Chat.Internal           (sendTurn)
import           Rockstar.Chat.Types
import qualified Rockstar.Codex                   as Codex
import           Rockstar.Credentials             (CredentialsM)
import           Rockstar.Interrupt               (Interrupted)
import           System.IO                        (hFlush, stderr, stdout)
import           System.IO.Error                  (isEOFError)

runChat :: Text -> CredentialsM ()
runChat selectedModel = Codex.withClient $ \manager -> do
    liftIO $ Text.putStrLn ("rockstar · " <> selectedModel <> " · reasoning max · fast on")
    liftIO $ Text.putStrLn "Type /exit to quit. Conversation history is not saved.\n"
    let display text = Text.putStr text >> hFlush stdout
        loop conversation = do
            liftIO $ display "> "
            input <- liftIO readInput
            case Text.strip <$> input of
                Nothing -> liftIO $ Text.putStrLn ""
                Just "/exit" -> pure ()
                Just "" -> loop conversation
                Just text -> do
                    result <- mapStateT onInterrupt $ sendTurn (Codex.fetchReply manager) conversation text display
                    liftIO $ Text.putStrLn "\n"
                    case result of
                        Right next -> loop next
                        Left Codex.AuthenticationRejected -> liftIO $ throwIO Codex.AuthenticationRejected
                        Left error' -> do
                            liftIO $ Text.hPutStrLn stderr ("error: " <> Text.pack (show error'))
                            loop conversation
        onInterrupt action = action `catch` \(interrupted :: Interrupted) -> Text.putStrLn "\n" >> throwIO interrupted
    loop (Conversation selectedModel [])

readInput :: IO (Maybe Text)
readInput =
    eofAware `catch` \interruption -> case interruption of
        UserInterrupt -> pure Nothing
        other         -> throwIO other
  where
    eofAware =
        (Just <$> Text.getLine) `catch` \error' ->
            if isEOFError error' then pure Nothing else throwIO error'
