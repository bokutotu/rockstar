module Main (main) where

import qualified AuthSpec
import qualified ChatSpec
import qualified CliSpec
import qualified CodexSpec
import           Control.Monad          (void)
import           Rockstar.Auth.Internal (loadChatAuthIn)
import           Rockstar.Auth.OAuth    (refreshAt)
import qualified Rockstar.Http          as Http
import           System.Environment     (getArgs, setEnv)
import           Test.Hspec             (hspec)
import           TestSupport            (testEnvironment)

main :: IO ()
main = do
    mapM_ (uncurry setEnv) testEnvironment
    args <- getArgs
    case args of
        ["--refresh-helper", directory, url] -> Http.withManager $ \manager ->
            void $ loadChatAuthIn directory (refreshAt url manager)
        _ -> hspec $ do
            AuthSpec.spec
            CodexSpec.spec
            ChatSpec.spec
            CliSpec.spec
