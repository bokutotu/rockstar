module Rockstar.Auth.Types (
    StoredCredentials (..),
    LoginAttempt (..),
    AuthStatus (..),
    needsRefresh,
    authStatusAt,
    toChatAuth,
    validCredentials,
    unixNow,
) where

import           Control.Exception     (throwIO)
import           Data.Aeson
import           Data.Text             (Text)
import qualified Data.Text             as Text
import           Data.Time.Clock.POSIX (getPOSIXTime)
import           Data.Word             (Word64)
import           Rockstar.Auth.Error
import           Rockstar.Chat.Types   (ChatAuth (..))

-- Private storage representation. Never derive Show for these secrets.
data StoredCredentials = StoredCredentials
    { storedAccessToken  :: Text
    , storedRefreshToken :: Text
    , expiresAt          :: Word64
    , storedAccountId    :: Text
    }
    deriving (Eq)

data LoginAttempt = LoginAttempt
    { csrfState    :: Text
    , pkceVerifier :: Text
    }

data AuthStatus = SignedOut | TokenUnexpired | RefreshRequired
    deriving (Eq, Show)

instance FromJSON StoredCredentials where
    parseJSON = withObject "credentials" $ \o -> do
        credentials <-
            StoredCredentials
                <$> o .: "access_token"
                <*> o .: "refresh_token"
                <*> o .: "expires_at"
                <*> o .: "account_id"
        if validCredentials credentials then pure credentials else fail "empty credential fields"

-- Serialization is used only for the private credential file.
instance ToJSON StoredCredentials where
    toJSON credentials =
        object
            [ "access_token" .= storedAccessToken credentials
            , "refresh_token" .= storedRefreshToken credentials
            , "expires_at" .= expiresAt credentials
            , "account_id" .= storedAccountId credentials
            ]

validCredentials :: StoredCredentials -> Bool
validCredentials credentials =
    expiresAt credentials > 0
        && all
            (not . Text.null)
            [storedAccessToken credentials, storedRefreshToken credentials, storedAccountId credentials]

needsRefresh :: Word64 -> StoredCredentials -> Bool
needsRefresh now credentials = toInteger (expiresAt credentials) <= toInteger now + 60

authStatusAt :: Word64 -> Maybe StoredCredentials -> AuthStatus
authStatusAt _ Nothing = SignedOut
authStatusAt now (Just credentials)
    | needsRefresh now credentials = RefreshRequired
    | otherwise = TokenUnexpired

toChatAuth :: StoredCredentials -> ChatAuth
toChatAuth credentials = ChatAuth (storedAccessToken credentials) (storedAccountId credentials)

unixNow :: IO Word64
unixNow = do
    now <- floor <$> getPOSIXTime :: IO Integer
    if now < 0 || now > toInteger (maxBound :: Word64)
        then throwIO InvalidClock
        else pure (fromInteger now)
