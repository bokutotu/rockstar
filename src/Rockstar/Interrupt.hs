module Rockstar.Interrupt (Interrupted (..)) where

import           Control.Exception (Exception)
import           Data.Text         (Text)
import qualified Data.Text         as Text

data Interrupted = Interrupted Text
instance Show Interrupted where
    show (Interrupted message) = Text.unpack message
instance Exception Interrupted
