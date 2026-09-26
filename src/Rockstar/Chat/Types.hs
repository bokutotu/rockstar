module Rockstar.Chat.Types (
    ChatAuth (..),
    Conversation (..),
    Reply (..),
    defaultModel,
    stageTurn,
    commitTurn,
) where

import           Data.Aeson (Value, object, (.=))
import           Data.Text  (Text)

-- A request needs neither a refresh token nor the credential file's expiration.
-- Deliberately no Show instance for authentication data.
data ChatAuth = ChatAuth
    { accessToken :: Text
    , accountId   :: Text
    }
    deriving (Eq)

data Conversation = Conversation
    { model :: Text
    , items :: [Value]
    }
    deriving (Eq, Show)

newtype Reply = Reply {outputItems :: [Value]}
    deriving (Eq, Show)

defaultModel :: Text
defaultModel = "gpt-6-astra"

stageTurn :: Conversation -> Text -> Conversation
stageTurn conversation input =
    conversation
        { items =
            items conversation
                <> [ object
                        [ "type" .= ("message" :: Text)
                        , "role" .= ("user" :: Text)
                        , "content" .= [object ["type" .= ("input_text" :: Text), "text" .= input]]
                        ]
                   ]
        }

commitTurn :: Conversation -> Reply -> Conversation
commitTurn conversation reply = conversation{items = items conversation <> outputItems reply}
