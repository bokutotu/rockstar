{-# LANGUAGE DeriveAnyClass #-}

module Rockstar.Credentials (Credentials (..), CredentialsM, save, load) where

import           Control.Monad.IO.Class           (liftIO)
import           Control.Monad.Trans.State.Strict (StateT, get, put)
import           Data.Aeson                       (FromJSON, ToJSON,
                                                   eitherDecodeStrict', encode)
import qualified Data.ByteString                  as BS
import qualified Data.ByteString.Lazy             as LBS
import           Data.Text                        (Text)
import           Data.Word                        (Word64)
import           GHC.Generics                     (Generic)
import           System.Directory                 (createDirectoryIfMissing,
                                                   getHomeDirectory)
import           System.FilePath                  (takeDirectory, (</>))
import           System.IO.Error                  (catchIOError,
                                                   isDoesNotExistError)
import           System.Posix.Files               (setFileMode)

data Credentials = Credentials
    { accessToken  :: Text
    , refreshToken :: Text
    , expiresAt    :: Word64
    , accountId    :: Text
    }
    deriving stock (Generic)
    deriving anyclass (FromJSON, ToJSON)

type CredentialsM = StateT (Maybe Credentials) IO

save :: Credentials -> CredentialsM ()
save credentials = do
    liftIO $ do
        path <- credentialPath
        let directory = takeDirectory path
        createDirectoryIfMissing True directory
        setFileMode directory 0o700
        LBS.writeFile path (encode credentials <> "\n")
        setFileMode path 0o600
    put (Just credentials)

load :: CredentialsM (Maybe Credentials)
load = do
    cached <- get
    case cached of
        Just _ -> pure cached
        Nothing -> do
            credentials <- liftIO $ do
                path <- credentialPath
                let readCredentials = do
                        bytes <- BS.readFile path
                        either (const $ ioError $ userError "Invalid auth.json") (pure . Just) (eitherDecodeStrict' bytes)
                readCredentials `catchIOError` \error' ->
                    if isDoesNotExistError error' then pure Nothing else ioError error'
            put credentials
            pure credentials

credentialPath :: IO FilePath
credentialPath = do
    home <- getHomeDirectory
    pure (home </> ".rockstar" </> "auth.json")
