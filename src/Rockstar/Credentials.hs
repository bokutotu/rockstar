module Rockstar.Credentials (Credentials (..)) where

import           Data.Text (Text)
import           Data.Word (Word64)

data Credentials = Credentials
    { accessToken  :: Text
    , refreshToken :: Text
    , expiresAt    :: Word64
    , accountId    :: Text
    }
