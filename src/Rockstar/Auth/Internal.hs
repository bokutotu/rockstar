module Rockstar.Auth.Internal (requireSignedIn, loadChatAuth, loadChatAuthIn) where

import           Control.Exception     (catch, throwIO)
import           Control.Monad         (when)
import           Data.Maybe            (isNothing)
import           Network.HTTP.Client   (Manager)
import           Rockstar.Auth.Error
import qualified Rockstar.Auth.OAuth   as OAuth
import           Rockstar.Auth.Storage
import           Rockstar.Auth.Types
import           Rockstar.Chat.Types   (ChatAuth)
import           Rockstar.Interrupt    (deferInterrupts)

requireSignedIn :: IO ()
requireSignedIn = do
    directory <- credentialDirectory
    credentials <- readCredentials directory
    when (isNothing credentials) $ throwIO NotSignedIn

loadChatAuth :: Manager -> IO ChatAuth
loadChatAuth manager = do
    directory <- credentialDirectory
    loadChatAuthIn directory (OAuth.refreshCredentials manager)

-- Internal dependency injection keeps tests away from real credentials and endpoints.
loadChatAuthIn :: FilePath -> (StoredCredentials -> IO StoredCredentials) -> IO ChatAuth
loadChatAuthIn directory refresh = deferInterrupts $ withLock directory $ do
    stored <- readCredentials directory >>= maybe (throwIO NotSignedIn) pure
    now <- unixNow
    if not (needsRefresh now stored)
        then pure (toChatAuth stored)
        else do
            updated <- refresh stored
            saveUnlocked directory updated `catch` (throwIO . RefreshPersistence)
            pure (toChatAuth updated)
