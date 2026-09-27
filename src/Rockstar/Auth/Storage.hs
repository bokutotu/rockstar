{-# LANGUAGE CApiFFI #-}

module Rockstar.Auth.Storage (
    credentialDirectory,
    readCredentials,
    saveLogin,
    saveUnlocked,
    withLock,
) where

import           Control.Concurrent     (threadDelay)
import           Control.Exception      (bracket, bracketOnError, catch,
                                         throwIO)
import           Control.Monad          (unless, void, when)
import           Data.Aeson             (eitherDecodeStrict', encode)
import           Data.Bits              ((.&.), (.|.))
import qualified Data.ByteString        as BS
import qualified Data.ByteString.Lazy   as LBS
import           Foreign.C.Error        (eAGAIN, eINTR, eWOULDBLOCK, getErrno,
                                         throwErrno)
import           Foreign.C.Types        (CInt (..))
import           GHC.Clock              (getMonotonicTimeNSec)
import           Rockstar.Auth.Error
import           Rockstar.Auth.Types
import           System.Directory       (getHomeDirectory, renameFile)
import           System.FilePath        ((</>))
import           System.IO              (hClose, hFlush, hSetBinaryMode)
import           System.IO.Error        (isAlreadyExistsError,
                                         isDoesNotExistError)
import           System.IO.Temp         (withTempFile)
import           System.Posix.Directory (createDirectory)
import           System.Posix.Files
import           System.Posix.IO
import           System.Posix.Types     (Fd (..), FileMode)
import           System.Posix.Unistd    (fileSynchronise)
import           System.Posix.User      (getEffectiveUserID)

foreign import capi unsafe "sys/file.h flock" flock :: CInt -> CInt -> IO CInt
foreign import capi unsafe "sys/file.h value LOCK_EX" lockExclusive :: CInt
foreign import capi unsafe "sys/file.h value LOCK_NB" lockNonblocking :: CInt

credentialDirectory :: IO FilePath
credentialDirectory = credentialIO "locate your home directory" $ (</> ".rockstar") <$> getHomeDirectory

readCredentials :: FilePath -> IO (Maybe StoredCredentials)
readCredentials directoryPath = credentialIO "read auth.json" $ do
    exists <- privateDirectory directoryPath False
    let path = directoryPath </> "auth.json"
    found <- if exists then checkExisting path else pure False
    if not found
        then pure Nothing
        else do
            bytes <- bracket (privateHandle path) hClose $ \handle -> BS.hGet handle (maxBytes + 1)
            when (BS.length bytes > maxBytes) $ throwIO CredentialFileTooLarge
            -- Aeson diagnostics can quote secrets, so discard them entirely.
            either (const $ throwIO InvalidCredentialFile) (pure . Just) (eitherDecodeStrict' bytes)
  where
    maxBytes = 256 * 1024
    privateHandle path = bracketOnError (openPrivate path False) closeFd fdToHandle

saveLogin :: FilePath -> StoredCredentials -> IO ()
saveLogin path credentials = withLock path (saveUnlocked path credentials)

-- Caller holds auth.lock throughout reread -> refresh -> save.
saveUnlocked :: FilePath -> StoredCredentials -> IO ()
saveUnlocked directoryPath credentials = credentialIO "save credentials" $ do
    unless (validCredentials credentials) $ throwIO InvalidCredentialFile
    let path = directoryPath </> "auth.json"
    void $ checkExisting path
    withTempFile directoryPath ".auth.tmp" $ \temporary handle -> do
        hSetBinaryMode handle True
        setFileMode temporary 0o600
        LBS.hPut handle (encode credentials <> "\n")
        hFlush handle
        hClose handle
        bracket (openPrivate temporary False) closeFd fileSynchronise
        renameFile temporary path
        syncDirectory directoryPath

-- Never unlink auth.lock: separate inodes would allow concurrent lock owners.

withLock :: FilePath -> IO a -> IO a
withLock directoryPath action = credentialIO "lock credential storage" $ do
    void $ privateDirectory directoryPath True
    let path = directoryPath </> "auth.lock"
    void $ checkExisting path
    bracket (openPrivate path True) closeFd $ \(Fd descriptor) -> do
        started <- getMonotonicTimeNSec
        let acquire = do
                result <- flock descriptor (lockExclusive .|. lockNonblocking)
                when (result /= 0) $ do
                    errno <- getErrno
                    if errno == eINTR
                        then acquire
                        else
                            if errno == eAGAIN || errno == eWOULDBLOCK
                                then do
                                    now <- getMonotonicTimeNSec
                                    when (now - started >= 30 * 1000000000) $ throwIO CredentialLockTimeout
                                    threadDelay 25000
                                    acquire
                                else throwErrno "flock"
        acquire
        action

privateDirectory :: FilePath -> Bool -> IO Bool
privateDirectory path create = do
    when create $
        createDirectory path 0o700 `catch` \error' ->
            unless (isAlreadyExistsError error') (throwIO error')
    metadata <- missingOK (getSymbolicLinkStatus path)
    case metadata of
        Nothing -> pure False
        Just info -> do
            unless (isDirectory info) $ throwIO (UnsafeStorage path "expected a real directory, not a symlink")
            checkPrivate info path 0o700
            pure True

checkExisting :: FilePath -> IO Bool
checkExisting path = do
    metadata <- missingOK (getSymbolicLinkStatus path)
    case metadata of
        Nothing   -> pure False
        Just info -> checkFile info path >> pure True

openPrivate :: FilePath -> Bool -> IO Fd
openPrivate path create =
    bracketOnError
        ( openFd
            path
            (if create then ReadWrite else ReadOnly)
            defaultFileFlags
                { creat = if create then Just 0o600 else Nothing
                , nofollow = True
                , nonBlock = True
                , cloexec = True
                }
        )
        closeFd
        (\fd -> getFdStatus fd >>= \info -> checkFile info path >> pure fd)

checkFile :: FileStatus -> FilePath -> IO ()
checkFile info path = do
    unless (isRegularFile info) $ throwIO (UnsafeStorage path "expected a regular file, not a symlink")
    checkPrivate info path 0o600
    unless (linkCount info == 1) $ throwIO (UnsafeStorage path "must not have hard links")

checkPrivate :: FileStatus -> FilePath -> FileMode -> IO ()
checkPrivate info path expected = do
    owner <- getEffectiveUserID
    unless (fileOwner info == owner) $ throwIO (UnsafeStorage path "must belong to the current user")
    unless (fileMode info .&. 0o777 == expected) $ throwIO (UnsafePermissions path (toInteger expected))

syncDirectory :: FilePath -> IO ()
syncDirectory path =
    bracket
        (openFd path ReadOnly defaultFileFlags{nofollow = True, directory = True, cloexec = True})
        closeFd
        fileSynchronise

missingOK :: IO a -> IO (Maybe a)
missingOK action =
    (Just <$> action) `catch` \error' ->
        if isDoesNotExistError error' then pure Nothing else throwIO error'
