module Rockstar.Chat.Internal (sendTurn, sanitizeOutput) where

import           Control.Exception                (AsyncException (UserInterrupt),
                                                   catch, throwIO)
import           Control.Monad.IO.Class           (liftIO)
import           Control.Monad.Trans.State.Strict (mapStateT)
import           Data.Char                        (isControl)
import           Data.Text                        (Text)
import qualified Data.Text                        as Text
import           Rockstar.Chat.Types
import           Rockstar.Codex                   (CodexError, replyText)
import           Rockstar.Credentials             (CredentialsM)
import           Rockstar.Interrupt               (Interrupted (..))

sendTurn
    :: (Conversation -> CredentialsM (Either CodexError Reply))
    -> Conversation
    -> Text
    -> (Text -> IO ())
    -> CredentialsM (Either CodexError Conversation)
sendTurn send conversation input display = mapStateT onInterrupt $ do
    let staged = stageTurn conversation input
    result <- send staged
    case result >>= (\reply -> (reply,) <$> replyText reply) of
        Left error' -> pure (Left error')
        Right (reply, text) -> do
            liftIO $ display (sanitizeOutput text)
            pure (Right $ commitTurn staged reply)
  where
    onInterrupt action =
        action `catch` \interruption -> case interruption of
            UserInterrupt -> throwIO (Interrupted "Interrupted; the incomplete turn was not saved")
            other -> throwIO other

sanitizeOutput :: Text -> Text
sanitizeOutput = Text.filter (\character -> not (isControl character) || character `elem` ['\n', '\t'])
