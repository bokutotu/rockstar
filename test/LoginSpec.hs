module LoginSpec (spec) where

import           Control.Concurrent.Async
import           Control.Exception
import           Control.Monad            (forM, forM_, void)
import           Data.Aeson
import           Data.Bifunctor           (first)
import qualified Data.ByteString.Lazy     as LBS
import           Data.IORef
import           Data.List                (sort)
import           Data.Text                (Text)
import qualified Data.Text                as Text
import           Data.Text.Encoding       (encodeUtf8)
import           Data.Time.Clock.POSIX    (getPOSIXTime)
import           Network.HTTP.Client      hiding (path)
import           Network.HTTP.Types
import           Network.Wai              (responseLBS, strictRequestBody)
import qualified Rockstar.Http            as Http
import           Rockstar.Login
import           System.IO.Error          (ioeGetErrorString, tryIOError)
import           Test.Hspec
import           TestSupport

spec :: Spec
spec = describe "login" $ do
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

    it "uses JWT expiry and falls back to ID-token accounts" $ do
        -- Arrange
        let jwt =
                LBS.toStrict $
                    encode $
                        object
                            [ "access_token" .= accessTokenFor 4600
                            , "refresh_token" .= ("refresh" :: Text)
                            ]
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
                [ Right $
                    object
                        [ "accessToken" .= accessTokenFor 4600
                        , "refreshToken" .= ("refresh" :: Text)
                        , "expiresAt" .= (4600 :: Int)
                        , "accountId" .= ("test-account" :: Text)
                        ]
                , Right $
                    object
                        [ "accessToken" .= ("opaque-token" :: Text)
                        , "refreshToken" .= ("refresh" :: Text)
                        , "expiresAt" .= (4600 :: Int)
                        , "accountId" .= ("test-account" :: Text)
                        ]
                ]

        -- Act
        let actual = map (fmap toJSON . parseTokens 1000) [jwt, opaque]

        -- Assert
        actual `shouldBe` expected

    it "rejects incomplete credentials and invalid expiration with safe diagnostics" $ do
        -- Arrange
        let missingAccount = object ["access_token" .= ("opaque-token" :: Text), "expires_in" .= (3600 :: Int)]
            missingExpiry = object ["access_token" .= ("opaque-token" :: Text), "id_token" .= accessTokenFor 4600]
            missingRefresh = object ["access_token" .= accessTokenFor 4600]
            emptyAccess =
                object
                    [ "access_token" .= ("" :: Text)
                    , "refresh_token" .= ("refresh" :: Text)
                    , "id_token" .= accessTokenFor 4600
                    , "expires_in" .= (3600 :: Int)
                    ]
            emptyRefresh = object ["access_token" .= accessTokenFor 4600, "refresh_token" .= ("" :: Text)]
            expired = object ["access_token" .= accessTokenFor 4600, "expires_in" .= (0 :: Int)]
            overflow = object ["access_token" .= accessTokenFor 4600, "expires_in" .= (18446744073709551615 :: Integer)]
            inputs =
                map
                    (LBS.toStrict . encode)
                    [missingAccount, missingExpiry, missingRefresh, emptyAccess, emptyRefresh, expired, overflow]
                    <> ["{\"access_token\": {\"secret\": \"do-not-print\"}}"]
            expected =
                [ Left "Invalid token endpoint response: missing ChatGPT account ID"
                , Left "Invalid token endpoint response: missing expiration"
                , Left "Invalid token endpoint response: missing refresh token"
                , Left "Invalid token endpoint response: empty credential fields"
                , Left "Invalid token endpoint response: empty credential fields"
                , Left "Invalid token endpoint response: token already expired; check the system clock"
                , Left "Invalid token endpoint response: invalid expiration"
                , Left "Invalid token endpoint response: malformed or missing token fields"
                ]
                    :: [Either String Value]

        -- Act
        let actual = map (fmap toJSON . parseTokens 1000) inputs

        -- Assert
        actual `shouldBe` expected

    it "reports rejected code exchanges without exposing response bodies" $ Http.withManager $ \manager ->
        forM_ [status400, status401, status500] $ \status -> do
            -- Arrange
            let app _ respond = respond (responseLBS status [] "secret upstream diagnostics")
                expected =
                    Left
                        ( "OpenAI rejected the authorization-code exchange (HTTP "
                            <> show (statusCode status)
                            <> "); run `rockstar login` again"
                        )
                        :: Either String Value

            -- Act
            let action = withServer app $ \url -> do
                    result <-
                        tryIOError $ toJSON <$> exchangeCodeAt url manager (LoginAttempt "state" "verifier") "code"
                    pure (first ioeGetErrorString result)

            -- Assert
            action `shouldReturn` expected

    it "never follows token-endpoint redirects" $ Http.withManager $ \manager -> do
        -- Arrange
        calls <- newIORef (0 :: Int)
        let expected =
                ( Left "OpenAI rejected the authorization-code exchange (HTTP 307); run `rockstar login` again"
                    :: Either String Value
                , 0
                )
            destination _ respond = modifyIORef' calls (+ 1) >> respond (responseLBS status200 [] "")

        -- Act
        let action = withServer destination $ \target -> do
                let origin _ respond = respond (responseLBS status307 [("Location", encodeUtf8 $ Text.pack target)] "")
                result <- withServer origin $ \url -> tryIOError $ toJSON <$> exchangeCodeAt url manager (LoginAttempt "state" "verifier") "code"
                count <- readIORef calls
                pure (first ioeGetErrorString result, count)

        -- Assert
        action `shouldReturn` expected
