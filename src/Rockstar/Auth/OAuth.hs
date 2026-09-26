module Rockstar.Auth.OAuth (
    beginLogin,
    authorizationUrl,
    pkceChallenge,
    withCallback,
    exchangeCode,
    exchangeCodeAt,
    refreshCredentials,
    refreshAt,
    parseTokens,
) where

import           Control.Applicative        ((<|>))
import           Control.Concurrent.Async   (race, waitCatch, withAsync)
import           Control.Concurrent.STM
import           Control.Exception          (IOException, bracket,
                                             bracketOnError, catch, throwIO)
import           Control.Monad              (unless, when)
import           Crypto.Hash                (Digest, SHA256, hash)
import           Data.Aeson
import qualified Data.Aeson.KeyMap          as KeyMap
import           Data.Aeson.Types           (parseMaybe)
import qualified Data.ByteArray             as ByteArray
import qualified Data.ByteString            as BS
import qualified Data.ByteString.Base64.URL as Base64
import qualified Data.ByteString.Lazy       as LBS
import           Data.Text                  (Text)
import qualified Data.Text                  as Text
import           Data.Text.Encoding         (decodeUtf8, decodeUtf8',
                                             encodeUtf8)
import           Data.Word                  (Word64)
import           Network.HTTP.Client        (Manager, responseBody,
                                             responseStatus, responseTimeout,
                                             responseTimeoutMicro,
                                             urlEncodedBody, withResponse)
import           Network.HTTP.Types
import qualified Network.Socket             as Socket
import           Network.Wai                (Application, pathInfo, queryString,
                                             requestMethod, responseLBS)
import           Network.Wai.Handler.Warp
import           Rockstar.Auth.Error
import           Rockstar.Auth.Types
import qualified Rockstar.Http              as Http
import           System.Entropy             (getEntropy)
import           System.Timeout             (timeout)

-- Public Codex client ID, not a client secret. The originator identifies rockstar.
clientId, redirectUri :: BS.ByteString
clientId = "app_EMoamEEZ73f0CkXaXp7hrann"
redirectUri = "http://localhost:1455/auth/callback"

tokenUrl :: String
tokenUrl = "https://auth.openai.com/oauth/token"

beginLogin :: IO LoginAttempt
beginLogin = LoginAttempt <$> randomSecret <*> randomSecret
  where
    randomSecret =
        (decodeUtf8 . Base64.encodeUnpadded <$> getEntropy 32)
            `catch` \(_ :: IOException) -> throwIO EntropyFailure

pkceChallenge :: Text -> BS.ByteString
pkceChallenge verifier = Base64.encodeUnpadded (ByteArray.convert (hash (encodeUtf8 verifier) :: Digest SHA256))

authorizationUrl :: LoginAttempt -> Text
authorizationUrl attempt =
    "https://auth.openai.com/oauth/authorize"
        <> decodeUtf8
            ( renderSimpleQuery
                True
                [ ("response_type", "code")
                , ("client_id", clientId)
                , ("redirect_uri", redirectUri)
                , ("scope", "openid profile email offline_access")
                , ("code_challenge", pkceChallenge (pkceVerifier attempt))
                , ("code_challenge_method", "S256")
                , ("state", encodeUtf8 (csrfState attempt))
                , ("id_token_add_organizations", "true")
                , ("codex_cli_simplified_flow", "true")
                , ("originator", "rockstar")
                ]
            )

exchangeCode :: Manager -> LoginAttempt -> Text -> IO StoredCredentials
exchangeCode = exchangeCodeAt tokenUrl

exchangeCodeAt :: String -> Manager -> LoginAttempt -> Text -> IO StoredCredentials
exchangeCodeAt url manager attempt code =
    requestTokens
        url
        manager
        Nothing
        [ ("grant_type", "authorization_code")
        , ("client_id", clientId)
        , ("code", encodeUtf8 code)
        , ("code_verifier", encodeUtf8 (pkceVerifier attempt))
        , ("redirect_uri", redirectUri)
        ]

refreshCredentials :: Manager -> StoredCredentials -> IO StoredCredentials
refreshCredentials = refreshAt tokenUrl

refreshAt :: String -> Manager -> StoredCredentials -> IO StoredCredentials
refreshAt url manager previous =
    requestTokens
        url
        manager
        (Just previous)
        [ ("grant_type", "refresh_token")
        , ("client_id", clientId)
        , ("refresh_token", encodeUtf8 (storedRefreshToken previous))
        ]

requestTokens
    :: String
    -> Manager
    -> Maybe StoredCredentials
    -> [(BS.ByteString, BS.ByteString)]
    -> IO StoredCredentials
requestTokens url manager previous form = do
    now <- unixNow
    let operation = case previous of
            Nothing -> "exchanging the authorization code"
            Just _  -> "refreshing credentials"
    result <-
        ( timeout (30 * 1000000) $ Http.network operation $ do
            base <- Http.request url
            let request = (urlEncodedBody form base){responseTimeout = responseTimeoutMicro (30 * 1000000)}
            withResponse request manager $ \response -> do
                let code = statusCode (responseStatus response)
                unless (code >= 200 && code < 300) $ throwIO $ case previous of
                    Nothing -> TokenExchangeRejected code
                    Just _
                        | code `elem` [400, 401, 403] -> RefreshRejected code
                        | otherwise -> TokenEndpointUnavailable code
                body <- Http.limitedBody (256 * 1024) (responseBody response)
                either throwIO pure (parseTokens now previous body)
        )
            `catch` \(_ :: Http.HttpError) -> throwIO (AuthNetwork operation)
    maybe (throwIO $ AuthNetwork operation) pure result

data TokenResponse = TokenResponse Text (Maybe Text) (Maybe Text) (Maybe Word64)
instance FromJSON TokenResponse where
    parseJSON = withObject "token response" $ \o ->
        TokenResponse
            <$> o .: "access_token"
            <*> o .:? "refresh_token"
            <*> o .:? "id_token"
            <*> o .:? "expires_in"

parseTokens
    :: Word64 -> Maybe StoredCredentials -> BS.ByteString -> Either AuthError StoredCredentials
parseTokens now previous body = do
    TokenResponse access refresh idToken ttl <-
        either
            (const $ Left $ InvalidTokenResponse "malformed or missing token fields")
            Right
            (eitherDecodeStrict' body)
    let accessClaims = jwtMetadata access
        idClaims = idToken >>= jwtMetadata
        jwtExpiry = accessClaims >>= field "exp" >>= parseMaybe parseJSON
    account <-
        maybe
            (Left $ InvalidTokenResponse "missing ChatGPT account ID")
            Right
            ((accessClaims >>= claimAccount) <|> (idClaims >>= claimAccount) <|> (storedAccountId <$> previous))
    when (maybe False ((/= account) . storedAccountId) previous) $ Left TokenAccountChanged
    expiry <- case (ttl, jwtExpiry) of
        (Just seconds, jwt) -> do
            let total = toInteger now + toInteger seconds
            when (total > toInteger (maxBound :: Word64)) $ Left (InvalidTokenResponse "invalid expiration")
            pure $ maybe (fromInteger total) (min (fromInteger total)) jwt
        (Nothing, Just value) -> pure value
        (Nothing, Nothing) -> Left (InvalidTokenResponse "missing expiration")
    when (expiry <= now) $ Left (InvalidTokenResponse "token already expired; check the system clock")
    refreshToken <-
        maybe
            (Left $ InvalidTokenResponse "missing refresh token")
            Right
            (refresh <|> (storedRefreshToken <$> previous))
    let credentials = StoredCredentials access refreshToken expiry account
    unless (validCredentials credentials) $ Left (InvalidTokenResponse "empty credential fields")
    pure credentials

-- Only metadata from the trusted HTTPS token endpoint, not verified identity claims.
jwtMetadata :: Text -> Maybe Value
jwtMetadata token = case Text.splitOn "." token of
    [_, payload, _] ->
        either
            (const Nothing)
            decodeStrict'
            (Base64.decodeUnpadded $ encodeUtf8 $ Text.dropWhileEnd (== '=') payload)
    _ -> Nothing

field :: Key -> Value -> Maybe Value
field key (Object object') = KeyMap.lookup key object'
field _ _                  = Nothing

claimAccount :: Value -> Maybe Text
claimAccount claims = do
    value <-
        (field "https://api.openai.com/auth" claims >>= field "chatgpt_account_id")
            <|> field "chatgpt_account_id" claims
    text <- parseMaybe parseJSON value
    if Text.null text then Nothing else Just text

-- Bind before displaying the authorization URL. Tests use port 0 on loopback.
withCallback :: LoginAttempt -> Int -> (Int -> IO Text -> IO a) -> IO a
withCallback attempt port action = bracket open Socket.close $ \socket -> do
    actualPort <-
        Socket.getSocketName socket >>= \case
            Socket.SockAddrInet number _ -> pure (fromIntegral number)
            _ -> throwIO CallbackClosed
    result <- newEmptyTMVarIO
    let settings = setOnException (\_ _ -> pure ()) $ setTimeout 30 defaultSettings
    withAsync (runSettingsSocket settings socket (callback attempt result)) $ \server -> do
        let waitForCode = do
                received <- timeout (600 * 1000000) $ race (waitCatch server) (atomically $ readTMVar result)
                case received of
                    Nothing           -> throwIO LoginTimedOut
                    Just (Left _)     -> throwIO CallbackClosed
                    Just (Right code) -> either throwIO pure code
        action actualPort waitForCode
  where
    open =
        ( bracketOnError (Socket.socket Socket.AF_INET Socket.Stream Socket.defaultProtocol) Socket.close $ \socket -> do
            Socket.setSocketOption socket Socket.ReuseAddr 1
            Socket.bind
                socket
                (Socket.SockAddrInet (fromIntegral port) (Socket.tupleToHostAddress (127, 0, 0, 1)))
            Socket.listen socket 128
            pure socket
        )
            `catch` (throwIO . CallbackBind)

callback :: LoginAttempt -> TMVar (Either AuthError Text) -> Application
callback attempt result request respond
    | pathInfo request /= ["auth", "callback"] = reply status404 "Not found."
    | requestMethod request /= "GET" = reply status405 "Method not allowed."
    | values "state" /= [Just (encodeUtf8 (csrfState attempt))] =
        reply status400 "Invalid OAuth state. Return to the terminal and try again."
    | otherwise = case outcome of
        Nothing -> reply status400 "Missing or ambiguous authorization code."
        Just code -> do
            accepted <- atomically $ tryPutTMVar result code
            if not accepted
                then reply status409 "This authorization attempt was already received."
                else case code of
                    Left _ -> reply status400 "Authorization was denied. Return to the terminal."
                    Right _ -> reply status200 "Authorization received. Return to the terminal to finish signing in."
  where
    values name = [value | (key, value) <- queryString request, key == name]
    outcome
        | not (null (values "error")) = Just (Left AuthorizationDenied)
        | otherwise = case values "code" of
            [Just code] | not (BS.null code) -> either (const Nothing) (Just . Right) (decodeUtf8' code)
            _ -> Nothing
    reply code message =
        respond
            (responseLBS code [("Content-Type", "text/html; charset=utf-8")] (message :: LBS.ByteString))
