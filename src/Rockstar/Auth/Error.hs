module Rockstar.Auth.Error (AuthError (..), credentialIO) where

import           Control.Exception (Exception, IOException, catch, throwIO)
import           Data.Text         (Text)
import qualified Data.Text         as Text
import           Numeric           (showOct)

-- Only safe diagnostics belong here, never token responses or parser errors.
data AuthError
    = NotSignedIn
    | InvalidCredentialFile
    | CredentialFileTooLarge
    | CredentialIO Text IOException
    | UnsafeStorage FilePath Text
    | UnsafePermissions FilePath Integer
    | CredentialLockTimeout
    | RefreshPersistence AuthError
    | InvalidTokenResponse Text
    | TokenExchangeRejected Int
    | RefreshRejected Int
    | TokenEndpointUnavailable Int
    | TokenAccountChanged
    | CallbackBind IOException
    | CallbackClosed
    | LoginTimedOut
    | AuthorizationDenied
    | AuthNetwork Text
    | InvalidClock
    | EntropyFailure

instance Exception AuthError

instance Show AuthError where
    show NotSignedIn = "Not signed in. Run `rockstar login` first"
    show InvalidCredentialFile = "Invalid auth.json; run `rockstar login` to replace it"
    show CredentialFileTooLarge = "auth.json is too large"
    show (CredentialIO operation source) = "Could not " <> Text.unpack operation <> ": " <> show source
    show (UnsafeStorage path reason) = "Unsafe credential storage at " <> path <> ": " <> Text.unpack reason
    show (UnsafePermissions path expected) = "Unsafe permissions on " <> path <> "; set permissions to " <> showOct expected ""
    show CredentialLockTimeout = "Timed out waiting for another rockstar authentication operation"
    show (RefreshPersistence source) =
        "Tokens were refreshed, but saving them failed: "
            <> show source
            <> ". Run `rockstar login` before retrying"
    show (InvalidTokenResponse reason) = "Invalid token endpoint response: " <> Text.unpack reason
    show (TokenExchangeRejected code) =
        "OpenAI rejected the authorization-code exchange (HTTP "
            <> show code
            <> "); run `rockstar login` again"
    show (RefreshRejected code) = "OpenAI rejected the refresh token (HTTP " <> show code <> "); run `rockstar login` again"
    show (TokenEndpointUnavailable code) = "Token endpoint failed (HTTP " <> show code <> "); existing credentials were preserved"
    show TokenAccountChanged = "Refreshed credentials belong to a different account; run `rockstar login` again"
    show (CallbackBind source) = "Could not bind the OAuth callback listener on 127.0.0.1:1455: " <> show source
    show CallbackClosed = "OAuth callback server stopped before authorization completed"
    show LoginTimedOut = "Login timed out; run `rockstar login` again"
    show AuthorizationDenied = "OpenAI authorization was denied or cancelled"
    show (AuthNetwork operation) = "Network error during " <> Text.unpack operation
    show InvalidClock = "System clock is before the Unix epoch or out of range"
    show EntropyFailure = "Could not obtain secure random bytes"

credentialIO :: Text -> IO a -> IO a
credentialIO operation action = action `catch` (throwIO . CredentialIO operation)
