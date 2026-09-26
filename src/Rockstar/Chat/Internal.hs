module Rockstar.Chat.Internal (sendTurn, sanitizeOutput) where

import           Control.Exception   (AsyncException (UserInterrupt), catch,
                                      throwIO)
import           Data.Char           (isControl)
import           Data.Text           (Text)
import qualified Data.Text           as Text
import           Rockstar.Chat.Types
import           Rockstar.Codex      (replyText)
import           Rockstar.Interrupt  (Interrupted (..))

sendTurn
    :: (Conversation -> IO Reply)
    -> Conversation
    -> Text
    -> (Text -> IO ())
    -> IO Conversation
sendTurn send conversation input display =
    turn `catch` \interruption -> case interruption of
        UserInterrupt -> throwIO (Interrupted "Interrupted; the incomplete turn was not saved")
        other -> throwIO other
  where
    turn = do
        let staged = stageTurn conversation input
        reply <- send staged
        text <- either throwIO pure (replyText reply)
        display (sanitizeOutput text)
        pure (commitTurn staged reply)

sanitizeOutput :: Text -> Text
sanitizeOutput = Text.filter (\character -> not (isControl character) || character `elem` ['\n', '\t'])
