module SpecHook (hook) where

import           System.Environment (setEnv)
import           Test.Hspec         (Spec, beforeAll_)
import           TestSupport        (testEnvironment)

hook :: Spec -> Spec
hook = beforeAll_ (mapM_ (uncurry setEnv) testEnvironment)
