module Rockstar.Interrupt (Interrupted (..), deferInterrupts, finishOnUserInterrupt) where

import           Control.Concurrent.Async (async, waitCatch)
import           Control.Exception
import           Data.Maybe               (fromMaybe)
import           Data.Text                (Text)
import qualified Data.Text                as Text

data Interrupted = Interrupted Text
instance Show Interrupted where
    show (Interrupted message) = Text.unpack message
instance Exception Interrupted

-- Join the worker even when the caller is interrupted. Unlike uninterruptibleMask,
-- this leaves the worker's network timeouts and exception cleanup operational.
-- Used only for bounded token exchanges followed by durable persistence.
deferInterrupts :: IO a -> IO a
deferInterrupts = finishTransaction throwIO

-- Once login has exchanged a code and saved credentials, report success rather
-- than claiming it was cancelled. Other asynchronous exceptions still propagate.
finishOnUserInterrupt :: IO a -> IO a
finishOnUserInterrupt = finishTransaction $ \exception ->
    case fromException exception of
        Just UserInterrupt -> pure ()
        _                  -> throwIO exception

finishTransaction :: (SomeException -> IO ()) -> IO a -> IO a
finishTransaction onInterrupt action = mask $ \restore -> do
    worker <- async (restore action)
    let join pending = do
            result <- try (waitCatch worker)
            case result of
                Left (interruption :: SomeException) -> join (Just (fromMaybe interruption pending))
                Right (Left failure) -> throwIO failure
                Right (Right value) -> maybe (pure ()) onInterrupt pending >> pure value
    join Nothing
