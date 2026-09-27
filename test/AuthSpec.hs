module AuthSpec (spec) where

import           Control.Concurrent.Async
import           Control.Exception
import           Control.Monad                    (forM, void)
import           Control.Monad.IO.Class           (liftIO)
import           Control.Monad.Trans.State.Strict (evalStateT, runStateT)
import           Data.Aeson
import           Data.Bifunctor                   (first)
import qualified Data.ByteString                  as BS
import qualified Data.ByteString.Base64.URL       as Base64
import qualified Data.ByteString.Lazy             as LBS
import           Data.IORef
import           Data.List                        (sort)
import           Data.Text                        (Text)
import qualified Data.Text                        as Text
import           Data.Text.Encoding               (decodeUtf8, encodeUtf8)
import           Data.Time.Clock.POSIX            (getPOSIXTime)
import           Network.HTTP.Client              hiding (path)
import           Network.HTTP.Types
import           Network.Wai                      (responseLBS,
                                                   strictRequestBody)
import           Rockstar.Auth                    (loadCredentials,
                                                   requireSignedIn)
import           Rockstar.Auth.OAuth
import           Rockstar.Credentials             (Credentials (..), save)
import qualified Rockstar.Http                    as Http
import           System.Directory                 (doesPathExist, removeFile)
import           System.FilePath                  ((</>))
import           System.IO.Error                  (ioeGetErrorString,
                                                   tryIOError)
import           Test.Hspec
import           TestSupport

spec :: Spec
spec = do
    describe "credential-backed authentication" $ do
        it "reports missing login without creating storage" $ withHome $ \home -> Http.withManager $ \manager -> do
            -- Arrange
            let expected =
                    ( Left "Not signed in. Run `rockstar login` first" :: Either String ()
                    , Left "Not signed in. Run `rockstar login` first" :: Either String Value
                    , False
                    )

            -- Act
            let action = do
                    required <- tryIOError $ evalStateT requireSignedIn Nothing
                    loaded <- tryIOError $ toJSON <$> evalStateT (loadCredentials manager) Nothing
                    exists <- doesPathExist (home </> ".rockstar")
                    pure (first ioeGetErrorString required, first ioeGetErrorString loaded, exists)

            -- Assert
            action `shouldReturn` expected

        it "uses cached unexpired credentials without accessing storage or the network" $ withHome $ \_ -> Http.withManager $ \manager -> do
            -- Arrange
            let input = Credentials "cached-access" "cached-refresh" maxBound "test-account"
                expectedCredentials =
                    object
                        [ "accessToken" .= ("cached-access" :: Text)
                        , "refreshToken" .= ("cached-refresh" :: Text)
                        , "expiresAt" .= (18446744073709551615 :: Integer)
                        , "accountId" .= ("test-account" :: Text)
                        ]
                expected = (expectedCredentials, Just expectedCredentials)

            -- Act
            let action = do
                    (loaded, cached) <- runStateT (requireSignedIn >> loadCredentials manager) (Just input)
                    pure (toJSON loaded, toJSON <$> cached)

            -- Assert
            action `shouldReturn` expected

        it "refreshes expired credentials once, saving and caching the rotated token" $ withHome $ \home -> do
            -- Arrange
            now <- floor <$> getPOSIXTime
            received <- newIORef []
            let expiry = now + 3600
                token = accessTokenFor expiry
                input = credentials 1
                expectedCredentials =
                    object
                        [ "accessToken" .= token
                        , "refreshToken" .= ("rotated-refresh" :: Text)
                        , "expiresAt" .= expiry
                        , "accountId" .= ("test-account" :: Text)
                        ]
                expectedForm =
                    sort
                        [ ("grant_type", "refresh_token")
                        , ("client_id", "app_EMoamEEZ73f0CkXaXp7hrann")
                        , ("refresh_token", "test-refresh-token")
                        ]
                expected =
                    ( expectedCredentials
                    , expectedCredentials
                    , Just expectedCredentials
                    , Just expectedCredentials
                    , [expectedForm]
                    )
                app request respond = do
                    form <- parseSimpleQuery . LBS.toStrict <$> strictRequestBody request
                    modifyIORef' received (<> [sort form])
                    respond $
                        responseLBS status200 [] $
                            encode $
                                object
                                    [ "access_token" .= token
                                    , "refresh_token" .= ("rotated-refresh" :: Text)
                                    , "expires_in" .= (3600 :: Int)
                                    ]
            evalStateT (save input) Nothing

            -- Act
            let action = withServer app $ \url -> do
                    manager <- managerAt url
                    ((loaded, again, stored), cached) <-
                        runStateT
                            ( do
                                loaded <- loadCredentials manager
                                stored <- liftIO $ decodeStrict' <$> BS.readFile (home </> ".rockstar" </> "auth.json")
                                liftIO $ removeFile (home </> ".rockstar" </> "auth.json")
                                again <- loadCredentials manager
                                pure (loaded, again, stored)
                            )
                            Nothing
                    forms <- readIORef received
                    pure (toJSON loaded, toJSON again, stored, toJSON <$> cached, forms)

            -- Assert
            action `shouldReturn` expected

        it "preserves stored credentials when refresh fails" $ withHome $ \home -> do
            -- Arrange
            let input = credentials 1
                app _ respond = respond (responseLBS status400 [] "secret upstream diagnostics")
            evalStateT (save input) Nothing
            original <- BS.readFile (home </> ".rockstar" </> "auth.json")
            let expected =
                    ( Left "OpenAI rejected the refresh token (HTTP 400); run `rockstar login` again"
                        :: Either String Value
                    , original
                    )

            -- Act
            let action = withServer app $ \url -> do
                    manager <- managerAt url
                    result <- tryIOError $ toJSON <$> evalStateT (loadCredentials manager) Nothing
                    stored <- BS.readFile (home </> ".rockstar" </> "auth.json")
                    pure (first ioeGetErrorString result, stored)

            -- Assert
            action `shouldReturn` expected

    describe "OAuth" $ do
        it "matches the RFC7636 PKCE example" $ do
            -- Arrange
            let input = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
                expected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"

            -- Act
            let actual = pkceChallenge input

            -- Assert
            actual `shouldBe` expected

        it "sends a challenge rather than the verifier in the authorization URL" $ do
            -- Arrange
            let input = LoginAttempt "test-state" "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
                expected =
                    sort
                        [ ("response_type", "code")
                        , ("client_id", "app_EMoamEEZ73f0CkXaXp7hrann")
                        , ("redirect_uri", "http://localhost:1455/auth/callback")
                        , ("scope", "openid profile email offline_access")
                        , ("code_challenge", "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM")
                        , ("code_challenge_method", "S256")
                        , ("state", "test-state")
                        , ("id_token_add_organizations", "true")
                        , ("codex_cli_simplified_flow", "true")
                        , ("originator", "rockstar")
                        ]

            -- Act
            let actual =
                    sort $ parseSimpleQuery $ encodeUtf8 $ Text.drop 1 $ snd $ Text.breakOn "?" $ authorizationUrl input

            -- Assert
            actual `shouldBe` expected

        it "rejects bad and duplicate callbacks without consuming the login attempt" $ Http.withManager $ \manager -> do
            -- Arrange
            let attempt = LoginAttempt "test-state" "test-verifier"
                queries =
                    [ "?state=wrong&code=bad"
                    , "?state=test-state&state=wrong&code=bad"
                    , "?state=test-state&code=one&code=two"
                    , "?state=test-state&code=test-code"
                    , "?state=test-state&code=test-code"
                    ]
                expected =
                    (
                        [ (400, "Invalid OAuth state. Return to the terminal and try again.")
                        , (400, "Invalid OAuth state. Return to the terminal and try again.")
                        , (400, "Missing or ambiguous authorization code.")
                        , (200, "Authorization received. Return to the terminal to finish signing in.")
                        , (409, "This authorization attempt was already received.")
                        ]
                    , "test-code"
                    )

            -- Act
            let action = withCallback attempt 0 $ \port waitCode -> do
                    responses <- forM queries $ \query -> do
                        request <- Http.request ("http://127.0.0.1:" <> show port <> "/auth/callback" <> query)
                        response <- httpLbs request manager
                        pure (statusCode $ responseStatus response, responseBody response)
                    code <- within waitCode
                    pure (responses, code)

            -- Assert
            action `shouldReturn` expected

        it "reports denial and closes the listener on cancellation" $ Http.withManager $ \manager -> do
            -- Arrange
            let attempt = LoginAttempt "state" "verifier"
                expected =
                    ( Left "OpenAI authorization was denied or cancelled" :: Either String Text
                    , Left (Just UserInterrupt) :: Either (Maybe AsyncException) Text
                    )

            -- Act
            let action = do
                    (port, denied) <- withCallback attempt 0 $ \port waitCode -> do
                        request <-
                            Http.request ("http://127.0.0.1:" <> show port <> "/auth/callback?state=state&error=access_denied")
                        void $ httpLbs request manager
                        result <- first ioeGetErrorString <$> tryIOError waitCode
                        pure (port, result)
                    interrupted <- withCallback attempt port $ \_ waitCode -> withAsync waitCode $ \worker -> do
                        throwTo (asyncThreadId worker) UserInterrupt
                        first fromException <$> within (waitCatch worker)
                    pure (denied, interrupted)

            -- Assert
            action `shouldReturn` expected

        it "exchanges codes with the complete PKCE form and no client secret" $ Http.withManager $ \manager -> do
            -- Arrange
            now <- floor <$> getPOSIXTime
            received <- newIORef []
            let expiry = now + 1800
                token = accessTokenFor expiry
                expectedCredentials =
                    object
                        [ "accessToken" .= token
                        , "refreshToken" .= ("test-refresh" :: Text)
                        , "expiresAt" .= expiry
                        , "accountId" .= ("test-account" :: Text)
                        ]
                expected =
                    ( expectedCredentials
                    , sort
                        [ ("grant_type", "authorization_code")
                        , ("client_id", "app_EMoamEEZ73f0CkXaXp7hrann")
                        , ("code", "test-code")
                        , ("code_verifier", "test-verifier")
                        , ("redirect_uri", "http://localhost:1455/auth/callback")
                        ]
                    )
                app request respond = do
                    form <- parseSimpleQuery . LBS.toStrict <$> strictRequestBody request
                    writeIORef received form
                    respond $
                        responseLBS status200 [] $
                            encode $
                                object
                                    [ "access_token" .= token
                                    , "refresh_token" .= ("test-refresh" :: Text)
                                    , "expires_in" .= (3600 :: Int)
                                    ]

            -- Act
            let action = withServer app $ \url -> do
                    result <- exchangeCodeAt url manager (LoginAttempt "state" "test-verifier") "test-code"
                    form <- readIORef received
                    pure (toJSON result, sort form)

            -- Assert
            action `shouldReturn` expected

        it "retains refresh tokens, uses JWT expiry, and falls back to ID-token accounts" $ do
            -- Arrange
            let previous = credentials 3000
                body = LBS.toStrict $ encode $ object ["access_token" .= accessTokenFor 4600]
                opaque =
                    LBS.toStrict $
                        encode $
                            object
                                [ "access_token" .= ("opaque-token" :: Text)
                                , "refresh_token" .= ("refresh" :: Text)
                                , "id_token" .= accessTokenFor 4600
                                , "expires_in" .= (3600 :: Int)
                                ]
                expected =
                    ( Right $
                        object
                            [ "accessToken" .= accessTokenFor 4600
                            , "refreshToken" .= ("test-refresh-token" :: Text)
                            , "expiresAt" .= (4600 :: Int)
                            , "accountId" .= ("test-account" :: Text)
                            ]
                    , Left "Invalid token endpoint response: missing refresh token" :: Either String Value
                    , Right $
                        object
                            [ "accessToken" .= ("opaque-token" :: Text)
                            , "refreshToken" .= ("refresh" :: Text)
                            , "expiresAt" .= (4600 :: Int)
                            , "accountId" .= ("test-account" :: Text)
                            ]
                    )

            -- Act
            let actual =
                    ( toJSON <$> parseTokens 1000 (Just previous) body
                    , toJSON <$> parseTokens 1000 Nothing body
                    , toJSON <$> parseTokens 1000 Nothing opaque
                    )

            -- Assert
            actual `shouldBe` expected

        it "rejects account changes and invalid expiration with safe diagnostics" $ do
            -- Arrange
            let token =
                    "header."
                        <> decodeUtf8
                            ( Base64.encodeUnpadded $
                                LBS.toStrict $
                                    encode $
                                        object ["chatgpt_account_id" .= ("different-account" :: Text)]
                            )
                        <> ".signature"
                changed = LBS.toStrict $ encode $ object ["access_token" .= token, "expires_in" .= (3600 :: Int)]
                expired =
                    LBS.toStrict $ encode $ object ["access_token" .= accessTokenFor 4600, "expires_in" .= (0 :: Int)]
                overflow =
                    LBS.toStrict $
                        encode $
                            object ["access_token" .= accessTokenFor 4600, "expires_in" .= (18446744073709551615 :: Integer)]
                inputs = [changed, expired, overflow, "{\"access_token\": {\"secret\": \"do-not-print\"}}"]
                expected =
                    [ Left "Refreshed credentials belong to a different account; run `rockstar login` again"
                    , Left "Invalid token endpoint response: token already expired; check the system clock"
                    , Left "Invalid token endpoint response: invalid expiration"
                    , Left "Invalid token endpoint response: malformed or missing token fields"
                    ]
                        :: [Either String Value]

            -- Act
            let actual = map (fmap toJSON . parseTokens 1000 (Just $ credentials 4600)) inputs

            -- Assert
            actual `shouldBe` expected

        it "never follows token-endpoint redirects" $ Http.withManager $ \manager -> do
            -- Arrange
            calls <- newIORef (0 :: Int)
            let expected =
                    ( Left "Token endpoint failed (HTTP 307); existing credentials were preserved" :: Either String Value
                    , 0
                    )
                destination _ respond = modifyIORef' calls (+ 1) >> respond (responseLBS status200 [] "")

            -- Act
            let action = withServer destination $ \target -> do
                    let origin _ respond = respond (responseLBS status307 [("Location", encodeUtf8 $ Text.pack target)] "")
                    result <- withServer origin $ \url -> tryIOError $ toJSON <$> refreshAt url manager (credentials 4600)
                    count <- readIORef calls
                    pure (first ioeGetErrorString result, count)

            -- Assert
            action `shouldReturn` expected
