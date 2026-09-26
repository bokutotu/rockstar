module Rockstar.Chat (runChat) where

import           Control.Exception      (AsyncException (UserInterrupt), catch,
                                         throwIO, try)
import           Data.Text              (Text)
import qualified Data.Text              as Text
import qualified Data.Text.IO           as Text
import           Rockstar.Chat.Internal (sendTurn)
import           Rockstar.Chat.Types
import qualified Rockstar.Codex         as Codex
import           Rockstar.Interrupt     (Interrupted)
import           System.IO              (hFlush, stderr, stdout)
import           System.IO.Error        (isEOFError)

runChat :: Text -> IO ()
runChat selectedModel = Codex.withClient $ \manager -> do
    Text.putStrLn ("rockstar · " <> selectedModel <> " · reasoning max · fast on")
    Text.putStrLn "Type /exit to quit. Conversation history is not saved.\n"
    let display text = Text.putStr text >> hFlush stdout
        loop conversation = do
            display "> "
            input <- readInput
            case Text.strip <$> input of
                Nothing -> Text.putStrLn ""
                Just "/exit" -> pure ()
                Just "" -> loop conversation
                Just text -> do
                    result <-
                        try (sendTurn (Codex.fetchReply manager) conversation text display)
                            `catch` \(interrupted :: Interrupted) -> Text.putStrLn "\n" >> throwIO interrupted
                    Text.putStrLn "\n"
                    case result of
                        Right next -> loop next
                        Left Codex.AuthenticationRejected -> throwIO Codex.AuthenticationRejected
                        Left (error' :: Codex.CodexError) -> do
                            Text.hPutStrLn stderr ("error: " <> Text.pack (show error'))
                            loop conversation
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
