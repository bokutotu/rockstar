{-# OPTIONS_GHC -Wno-deprecations #-}

module Rockstar.Http (
    HttpError (..),
    withManager,
    request,
    network,
    limitedBody,
) where

import           Control.Exception       (Exception, bracket, catch, throwIO)
import qualified Data.ByteString         as BS
import           Data.Text               (Text)
import qualified Data.Text               as Text
import           Network.HTTP.Client     hiding (withManager)
import qualified Network.HTTP.Client     as HTTP
import           Network.HTTP.Client.TLS (tlsManagerSettings)

data HttpError = NetworkFailure Text | ResponseTooLarge Int
instance Show HttpError where
    show (NetworkFailure operation) = "Network error during " <> Text.unpack operation
    show (ResponseTooLarge limit) = "HTTP response exceeded its " <> show limit <> "-byte size limit"
instance Exception HttpError

withManager :: (Manager -> IO a) -> IO a
withManager =
    bracket
        ( newManager
            tlsManagerSettings
                { managerResponseTimeout = responseTimeoutMicro (120 * 1000000)
                , managerRetryableException = const False
                }
        )
        HTTP.closeManager

request :: String -> IO Request
request url = network "preparing an HTTP request" $ do
    base <- parseRequest url
    pure
        base
            { redirectCount = 0
            , checkResponse = \_ _ -> pure ()
            , requestHeaders = [("User-Agent", "rockstar/0.1.0")]
            }

-- HttpException contains the request, including authorization headers. Never show it.
network :: Text -> IO a -> IO a
network operation action = action `catch` \(_ :: HttpException) -> throwIO (NetworkFailure operation)

limitedBody :: Int -> BodyReader -> IO BS.ByteString
limitedBody limit reader = go 0 []
  where
    go size chunks = do
        chunk <- brRead reader
        if BS.null chunk
            then pure (BS.concat (reverse chunks))
            else do
                let total = size + BS.length chunk
                if total > limit
                    then throwIO (ResponseTooLarge limit)
                    else go total (chunk : chunks)
