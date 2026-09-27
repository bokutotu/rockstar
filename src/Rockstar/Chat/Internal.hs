module Rockstar.Chat.Internal (sendTurn, sanitizeOutput) where

import           Control.Exception   (AsyncException (UserInterrupt), catch,
                                      throwIO)
import           Data.Char           (isControl)
import           Data.Text           (Text)
import qualified Data.Text           as Text
import           Rockstar.Chat.Types
import           Rockstar.Codex      (CodexError, replyText)
import           Rockstar.Interrupt  (Interrupted (..))

sendTurn
    :: (Conversation -> IO (Either CodexError Reply))
    -> Conversation
    -> Text
    -> (Text -> IO ())
    -> IO (Either CodexError Conversation)
sendTurn send conversation input display = onInterrupt $ do
    let staged = stageTurn conversation input
    result <- send staged
    case result >>= (\reply -> (reply,) <$> replyText reply) of
        Left error' -> pure (Left error')
        Right (reply, text) -> do
            display (sanitizeOutput text)
            pure (Right $ commitTurn staged reply)
  where
    onInterrupt action =
        action `catch` \interruption -> case interruption of
            UserInterrupt -> throwIO (Interrupted "Interrupted; the incomplete turn was not saved")
            other -> throwIO other

sanitizeOutput :: Text -> Text
sanitizeOutput = Text.filter (\character -> not (isControl character) || character `elem` ['\n', '\t'])
