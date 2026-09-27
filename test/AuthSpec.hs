module AuthSpec (spec) where

import           Control.Concurrent         (newEmptyMVar, putMVar, takeMVar,
                                             threadDelay)
import           Control.Concurrent.Async
import           Control.Exception
import           Control.Monad              (forM_, void)
import           Data.Aeson
import           Data.Bits                  ((.&.))
import qualified Data.ByteString            as BS
import qualified Data.ByteString.Base64.URL as Base64
import qualified Data.ByteString.Lazy       as LBS
import           Data.IORef
import           Data.List                  (sort)
import           Data.Text                  (Text)
import qualified Data.Text                  as Text
import           Data.Text.Encoding         (decodeUtf8, encodeUtf8)
import           Network.HTTP.Client        hiding (path)
import           Network.HTTP.Types
import           Network.Wai                (responseLBS, strictRequestBody)
import           Rockstar.Auth.Error
import           Rockstar.Auth.Internal
import           Rockstar.Auth.OAuth
import           Rockstar.Auth.Storage
import           Rockstar.Auth.Types
import           Rockstar.Chat.Types        (ChatAuth (..))
import qualified Rockstar.Http              as Http
import           Rockstar.Interrupt         (finishOnUserInterrupt)
import           System.Directory
import           System.FilePath            (takeDirectory, (</>))
import           System.Posix.Files
import           Test.Hspec
import           TestSupport

spec :: Spec
spec = do
    describe "authentication models" $ do
        it "keeps storage credentials separate from request authentication" $ do
            let stored = credentials 1000
            toChatAuth stored == ChatAuth (accessTokenFor 1000) "test-account" `shouldBe` True
            map (\now -> authStatusAt now (Just stored)) [939, 940, 1001]
                `shouldBe` [TokenUnexpired, RefreshRequired, RefreshRequired]
            authStatusAt 939 Nothing `shouldBe` SignedOut

    describe "private credential storage" $ do
        it "does not create storage when credentials are absent" $ withCredentialDirectory $ \directory -> do
            -- Arrange
            let expected = (Nothing :: Maybe Value, False)

            -- Act
            let action = do
                    stored <- readCredentials directory
                    exists <- doesPathExist directory
                    pure (toJSON <$> stored, exists)

            -- Assert
            action `shouldReturn` expected

        it "round-trips credentials with private permissions" $ withCredentialDirectory $ \directory -> do
            -- Arrange
            let input = StoredCredentials "test-access-token" "test-refresh-token" 4600 "test-account"
                paths = [directory, directory </> "auth.json", directory </> "auth.lock"]
                expected =
                    ( Just $
                        object
                            [ "access_token" .= ("test-access-token" :: Text)
                            , "refresh_token" .= ("test-refresh-token" :: Text)
                            , "expires_at" .= (4600 :: Int)
                            , "account_id" .= ("test-account" :: Text)
                            ]
                    , [0o700, 0o600, 0o600]
                    )

            -- Act
            let action = do
                    saveLogin directory input
                    stored <- readCredentials directory
                    modes <- mapM (fmap ((.&. 0o777) . fileMode) . getFileStatus) paths
                    pure (toJSON <$> stored, modes)

            -- Assert
            action `shouldReturn` expected

        it "rejects corrupt and oversized files without quoting their contents" $ withCredentialDirectory $ \directory -> do
            saveLogin directory (credentials 4600)
            BS.writeFile (directory </> "auth.json") "{secret-do-not-print"
            readCredentials directory
                `shouldThrow` (\error' -> show (error' :: AuthError) == "Invalid auth.json; run `rockstar login` to replace it")
            BS.writeFile (directory </> "auth.json") (BS.replicate (256 * 1024 + 1) 32)
            readCredentials directory
                `shouldThrow` (\error' -> case error' of CredentialFileTooLarge -> True; _ -> False)

        it "rejects symlink directories and symlink, hardlinked, public, or FIFO files" $ withCredentialDirectory $ \directory -> do
            createSymbolicLink (takeDirectory directory) directory
            saveLogin directory (credentials 4600) `shouldThrow` unsafeStorage
            removeLink directory
            saveLogin directory (credentials 4600)
            let path = directory </> "auth.json"
                target = takeDirectory directory </> "untouched"
            BS.writeFile target "do not change"
            removeFile path
            createSymbolicLink target path
            readCredentials directory `shouldThrow` unsafeStorage
            saveLogin directory (credentials 4600) `shouldThrow` unsafeStorage
            BS.readFile target `shouldReturn` "do not change"
            removeLink path
            saveLogin directory (credentials 4600)
            setFileMode path 0o644
            readCredentials directory
                `shouldThrow` ( \error' -> case error' of
                                    UnsafePermissions actual 0o600 -> actual == path
                                    _ -> False
                              )
            setFileMode path 0o600
            createLink path (directory </> "hardlink")
            readCredentials directory `shouldThrow` unsafeStorage
            removeLink (directory </> "hardlink")
            removeFile path
            createNamedPipe path 0o600
            within (readCredentials directory) `shouldThrow` unsafeStorage

    describe "OAuth" $ do
        it "matches RFC7636 and sends a challenge, never the verifier" $ do
            pkceChallenge "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
                `shouldBe` "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
            attempt <- beginLogin
            other <- beginLogin
            let url = authorizationUrl attempt
                query = parseSimpleQuery (encodeUtf8 $ Text.drop 1 $ snd $ Text.breakOn "?" url)
            sort query
                `shouldBe` sort
                    [ ("response_type", "code")
                    , ("client_id", "app_EMoamEEZ73f0CkXaXp7hrann")
                    , ("redirect_uri", "http://localhost:1455/auth/callback")
                    , ("scope", "openid profile email offline_access")
                    , ("code_challenge", pkceChallenge (pkceVerifier attempt))
                    , ("code_challenge_method", "S256")
                    , ("state", encodeUtf8 (csrfState attempt))
                    , ("id_token_add_organizations", "true")
                    , ("codex_cli_simplified_flow", "true")
                    , ("originator", "rockstar")
                    ]
            Text.isInfixOf (pkceVerifier attempt) url `shouldBe` False
            csrfState attempt `shouldNotBe` csrfState other

        it "rejects bad/duplicate callbacks without consuming the login attempt" $ Http.withManager $ \manager -> do
            let attempt = LoginAttempt "test-state" "test-verifier"
            withCallback attempt 0 $ \port waitCode -> do
                let get query = do
                        request <- Http.request ("http://127.0.0.1:" <> show port <> "/auth/callback" <> query)
                        response <- httpLbs request manager
                        pure (statusCode $ responseStatus response, responseBody response)
                get "?state=wrong&code=bad"
                    `shouldReturn` (400, "Invalid OAuth state. Return to the terminal and try again.")
                get "?state=test-state&state=wrong&code=bad"
                    `shouldReturn` (400, "Invalid OAuth state. Return to the terminal and try again.")
                get "?state=test-state&code=one&code=two"
                    `shouldReturn` (400, "Missing or ambiguous authorization code.")
                get "?state=test-state&code=test-code"
                    `shouldReturn` (200, "Authorization received. Return to the terminal to finish signing in.")
                within waitCode `shouldReturn` "test-code"
                get "?state=test-state&code=test-code"
                    `shouldReturn` (409, "This authorization attempt was already received.")

        it "reports denial and closes the listener on cancellation" $ Http.withManager $ \manager -> do
            let attempt = LoginAttempt "state" "verifier"
            port <- withCallback attempt 0 $ \port waitCode -> do
                request <-
                    Http.request ("http://127.0.0.1:" <> show port <> "/auth/callback?state=state&error=access_denied")
                void $ httpLbs request manager
                waitCode `shouldThrow` (\error' -> case error' of AuthorizationDenied -> True; _ -> False)
                pure port
            withCallback attempt port $ \_ waitCode -> withAsync waitCode $ \worker -> do
                throwTo (asyncThreadId worker) UserInterrupt
                waitCatch worker >>= expectInterrupt

        it "exchanges codes with the complete PKCE form and no client secret" $ Http.withManager $ \manager -> do
            now <- unixNow
            let expiry = now + 1800
                token = accessTokenFor expiry
                expected = StoredCredentials token "test-refresh" expiry "test-account"
            received <- newIORef []
            let app request respond = do
                    form <- parseSimpleQuery . LBS.toStrict <$> strictRequestBody request
                    writeIORef received form
                    respond $
                        responseLBS
                            status200
                            [("Content-Type", "application/json")]
                            ( encode $
                                object
                                    [ "access_token" .= token
                                    , "refresh_token" .= ("test-refresh" :: Text)
                                    , "expires_in" .= (3600 :: Int)
                                    ]
                            )
            withServer app $ \url -> do
                actual <- exchangeCodeAt url manager (LoginAttempt "state" "test-verifier") "test-code"
                toJSON actual `shouldBe` toJSON expected
            sort <$> readIORef received
                `shouldReturn` sort
                    [ ("grant_type", "authorization_code")
                    , ("client_id", "app_EMoamEEZ73f0CkXaXp7hrann")
                    , ("code", "test-code")
                    , ("code_verifier", "test-verifier")
                    , ("redirect_uri", "http://localhost:1455/auth/callback")
                    ]

        it "retains refresh tokens, uses JWT expiry, and falls back to ID-token accounts" $ do
            let body = LBS.toStrict $ encode $ object ["access_token" .= accessTokenFor 4600]
            actual <- either throwIO pure $ parseTokens 1000 (Just $ credentials 3000) body
            toJSON actual `shouldBe` toJSON (credentials 4600)
            isAuthFailure (parseTokens 1000 Nothing body) `shouldBe` True
            let opaque =
                    LBS.toStrict $
                        encode $
                            object
                                [ "access_token" .= ("opaque-token" :: Text)
                                , "refresh_token" .= ("refresh" :: Text)
                                , "id_token" .= accessTokenFor 4600
                                , "expires_in" .= (3600 :: Int)
                                ]
            parsed <- either throwIO pure $ parseTokens 1000 Nothing opaque
            toJSON parsed `shouldBe` toJSON (StoredCredentials "opaque-token" "refresh" 4600 "test-account")

        it "rejects account changes and invalid expiration with safe diagnostics" $ do
            let token =
                    "header."
                        <> decodeUtf8
                            ( Base64.encodeUnpadded $
                                LBS.toStrict $
                                    encode $
                                        object
                                            ["chatgpt_account_id" .= ("different-account" :: Text)]
                            )
                        <> ".signature"
                changed = LBS.toStrict $ encode $ object ["access_token" .= token, "expires_in" .= (3600 :: Int)]
            showFailure (parseTokens 1000 (Just $ credentials 4600) changed)
                `shouldBe` "Refreshed credentials belong to a different account; run `rockstar login` again"
            forM_ [0, 18446744073709551615 :: Integer] $ \ttl -> do
                let body = LBS.toStrict $ encode $ object ["access_token" .= accessTokenFor 4600, "expires_in" .= ttl]
                isAuthFailure (parseTokens 1000 (Just $ credentials 4600) body) `shouldBe` True
            showFailure (parseTokens 0 Nothing "{\"access_token\": {\"secret\": \"do-not-print\"}}")
                `shouldBe` "Invalid token endpoint response: malformed or missing token fields"

        it "never follows token-endpoint redirects" $ Http.withManager $ \manager -> do
            calls <- newIORef (0 :: Int)
            let destination _ respond = modifyIORef' calls (+ 1) >> respond (responseLBS status200 [] "")
            withServer destination $ \target -> do
                let origin _ respond = respond (responseLBS status307 [("Location", encodeUtf8 $ Text.pack target)] "")
                withServer origin $ \url ->
                    refreshAt url manager (credentials 4600)
                        `shouldThrow` (\error' -> case error' of TokenEndpointUnavailable 307 -> True; _ -> False)
            readIORef calls `shouldReturn` 0

    describe "refresh transactions" $ do
        it "refreshes once across threads and saves the rotated token" $ withCredentialDirectory $ \directory -> do
            now <- unixNow
            let updated = (credentials $ now + 3600){storedRefreshToken = "rotated-refresh"}
            saveLogin directory (credentials 1)
            calls <- newIORef (0 :: Int)
            let refresh _ = atomicModifyIORef' calls (\n -> (n + 1, ())) >> threadDelay 100000 >> pure updated
            (a, b) <- concurrently (loadChatAuthIn directory refresh) (loadChatAuthIn directory refresh)
            (a == toChatAuth updated, b == toChatAuth updated) `shouldBe` (True, True)
            fmap toJSON <$> readCredentials directory `shouldReturn` Just (toJSON updated)
            readIORef calls `shouldReturn` 1

        it "persists the rotating token before propagating interruption" $ withCredentialDirectory $ \directory -> do
            now <- unixNow
            let updated = (credentials $ now + 3600){storedRefreshToken = "rotated-refresh"}
            saveLogin directory (credentials 1)
            started <- newEmptyMVar
            release <- newEmptyMVar
            let refresh _ = putMVar started () >> takeMVar release >> pure updated
            withAsync (loadChatAuthIn directory refresh) $ \updating -> do
                within $ takeMVar started
                throwTo (asyncThreadId updating) UserInterrupt
                maybe True (const False) <$> poll updating `shouldReturn` True
                throwTo (asyncThreadId updating) UserInterrupt
                maybe True (const False) <$> poll updating `shouldReturn` True
                putMVar release ()
                within (waitCatch updating) >>= expectInterrupt
            fmap toJSON <$> readCredentials directory `shouldReturn` Just (toJSON updated)

        it "reports success once an interrupted login transaction has been saved" $ withCredentialDirectory $ \directory -> do
            now <- unixNow
            let stored = credentials (now + 3600)
            started <- newEmptyMVar
            release <- newEmptyMVar
            let loginTransaction = finishOnUserInterrupt $ do
                    putMVar started ()
                    takeMVar release
                    saveLogin directory stored
            withAsync loginTransaction $ \loggingIn -> do
                within $ takeMVar started
                throwTo (asyncThreadId loggingIn) UserInterrupt
                maybe True (const False) <$> poll loggingIn `shouldReturn` True
                putMVar release ()
                within (wait loggingIn) `shouldReturn` ()
            fmap toJSON <$> readCredentials directory `shouldReturn` Just (toJSON stored)

        it "reports persistence failure after a successful token exchange" $ withCredentialDirectory $ \directory -> do
            now <- unixNow
            let path = directory </> "auth.json"
                original = credentials 1
                updated = (credentials $ now + 3600){storedRefreshToken = "rotated-refresh"}
                refresh _ = setFileMode path 0o644 >> pure updated
            saveLogin directory original
            loadChatAuthIn directory refresh
                `shouldThrow` ( \error' -> case error' of
                                    RefreshPersistence (UnsafePermissions actual 0o600) -> actual == path
                                    _ -> False
                              )
            setFileMode path 0o600
            fmap toJSON <$> readCredentials directory `shouldReturn` Just (toJSON original)

        it "preserves the credential file when refresh fails" $ withCredentialDirectory $ \directory -> Http.withManager $ \manager -> do
            saveLogin directory (credentials 1)
            originalBytes <- BS.readFile (directory </> "auth.json")
            let app _ respond = respond (responseLBS status400 [] "secret upstream diagnostics")
            withServer app $ \url ->
                loadChatAuthIn directory (refreshAt url manager)
                    `shouldThrow` ( \error' ->
                                        show (error' :: AuthError)
                                            == "OpenAI rejected the refresh token (HTTP 400); run `rockstar login` again"
                                  )
            BS.readFile (directory </> "auth.json") `shouldReturn` originalBytes

unsafeStorage :: AuthError -> Bool
unsafeStorage (UnsafeStorage _ _) = True
unsafeStorage _                   = False

isAuthFailure :: Either AuthError a -> Bool
isAuthFailure (Left _) = True
isAuthFailure _        = False

showFailure :: Either AuthError a -> String
showFailure (Left error') = show error'
showFailure _             = "unexpected success"

expectInterrupt :: Either SomeException a -> Expectation
expectInterrupt (Left exception) = fromException exception `shouldBe` Just UserInterrupt
expectInterrupt (Right _) = expectationFailure "expected interruption"
