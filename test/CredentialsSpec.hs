module CredentialsSpec (spec) where

import           Control.Exception                (bracket)
import           Control.Monad.Trans.State.Strict (evalStateT, runStateT)
import           Data.Aeson                       (Value, object, toJSON, (.=))
import           Data.Text                        (Text)
import           Rockstar.Credentials             (Credentials (..), load, save)
import           System.Directory                 (getHomeDirectory, removeFile)
import           System.Environment               (lookupEnv, setEnv, unsetEnv)
import           System.FilePath                  ((</>))
import           System.IO.Temp                   (withSystemTempDirectory)
import           Test.Hspec

spec :: Spec
spec = around_ withTemporaryHome $ describe "credentials" $ do
    it "loads and caches credentials when auth.json exists" $ do
        -- Arrange
        let input = Credentials "test-access-token" "test-refresh-token" 4600 "test-account"
            expectedCredentials =
                object
                    [ "accessToken" .= ("test-access-token" :: Text)
                    , "refreshToken" .= ("test-refresh-token" :: Text)
                    , "expiresAt" .= (4600 :: Int)
                    , "accountId" .= ("test-account" :: Text)
                    ]
            expected = (Just expectedCredentials, Just expectedCredentials, Just expectedCredentials)

        -- Act
        let action = do
                evalStateT (save input) Nothing
                (loaded, state) <- runStateT load Nothing
                home <- getHomeDirectory
                removeFile (home </> ".rockstar" </> "auth.json")
                (cached, finalState) <- runStateT load state
                pure (toJSON <$> loaded, toJSON <$> cached, toJSON <$> finalState)

        -- Assert
        action `shouldReturn` expected

    it "returns Nothing when auth.json does not exist" $ do
        -- Arrange
        let input = Nothing
            expected = (Nothing :: Maybe Value, Nothing :: Maybe Value)

        -- Act
        let action = do
                (loaded, state) <- runStateT load input
                pure (toJSON <$> loaded, toJSON <$> state)

        -- Assert
        action `shouldReturn` expected

withTemporaryHome :: IO a -> IO a
withTemporaryHome action = withSystemTempDirectory "rockstar-credentials-test" $ \home ->
    bracket
        (lookupEnv "HOME")
        (maybe (unsetEnv "HOME") (setEnv "HOME"))
        (\_ -> setEnv "HOME" home >> action)
