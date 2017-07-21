//! Provide a UpmError enum which can represent all of the errors that may be returned by upm
//! functions.

extern crate openssl;

use std::error;
use std::fmt;
use std::io;
use time;

/// The errors that may be returned by UPM functions are categorized into these enum variants.
#[derive(Debug)]
pub enum UpmError {
    ReadUnderrun,
    KeyIVGeneration,
    AccountParse(Option<String>),
    Io(io::Error),
    BadMagic,
    BadVersion(u8),
    Crypto(openssl::error::ErrorStack),
    BadPassword,
    InvalidFilename,
    TimeParseError(time::ParseError),
    Sync(String),
    NoDatabaseFilename,
    NoDatabasePassword,
    NoSyncURL,
    NoSyncCredentials,
    SyncDatabaseNotFound,
    Backup(String),
    FlatpackOverflow,
    DuplicateAccountName(String),
    // PathNotUnicode errors are expected to contain the "lossy" version of the path string, with
    // invalid sequences converted into replacement characters via Path::to_string_lossy().
    PathNotUnicode(String),
}

impl fmt::Display for UpmError {
    /// Provide human-readable descriptions of the errors.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            UpmError::ReadUnderrun => write!(f, "read underrun"),
            UpmError::KeyIVGeneration => write!(f, "cannot generate key/iv"),
            UpmError::AccountParse(Some(ref s)) => write!(f, "error parsing account: {}", s),
            UpmError::AccountParse(None) => write!(f, "error parsing account"),
            UpmError::Io(ref e) => write!(f, "IO error: {}", e),
            UpmError::BadMagic => write!(f, "Bad magic in file header."),
            UpmError::BadVersion(v) => write!(f, "Unsupported database version: {}", v),
            UpmError::Crypto(ref e) => write!(f, "Crypto error: {}", e),
            UpmError::BadPassword => write!(f, "The provided password is incorrect."),
            UpmError::InvalidFilename => write!(f, "The database file path is invalid."),
            UpmError::TimeParseError(e) => write!(f, "Time parsing error: {}", e),
            UpmError::Sync(ref s) => write!(f, "Sync error: {}", s),
            UpmError::NoDatabaseFilename => write!(f, "No database filename was supplied."),
            UpmError::NoDatabasePassword => write!(f, "No database password was supplied."),
            UpmError::NoSyncURL => write!(f, "No sync URL is configured for this database."),
            UpmError::NoSyncCredentials => write!(f, "No sync credentials were supplied."),
            UpmError::SyncDatabaseNotFound => write!(f, "The remote database was not present."),
            UpmError::Backup(ref s) => write!(f, "Error making backup; not saved: {}", s),
            UpmError::FlatpackOverflow => {
                write!(f, "Data exceeds flatpack record limit of 9999 bytes.")
            }
            UpmError::DuplicateAccountName(ref s) => {
                write!(f, "Duplicate account name detected: \"{}\"", s)
            }
            UpmError::PathNotUnicode(ref s) => write!(f, "Path is not valid Unicode: \"{}\".", s),
        }
    }
}

impl error::Error for UpmError {
    /// Provide terse descriptions of the errors.
    fn description(&self) -> &str {
        match *self {
            UpmError::ReadUnderrun => "read underrun",
            UpmError::KeyIVGeneration => "cannot generate key/iv",
            UpmError::AccountParse(_) => "cannot parse account",
            UpmError::Io(ref err) => error::Error::description(err),
            UpmError::BadMagic => "bad magic",
            UpmError::BadVersion(_) => "bad database version",
            UpmError::Crypto(_) => "OpenSSL error",
            UpmError::BadPassword => "bad password",
            UpmError::InvalidFilename => "invalid filename",
            UpmError::TimeParseError(_) => "time parsing error",
            UpmError::Sync(_) => "cannot sync",
            UpmError::NoDatabaseFilename => "no database filename",
            UpmError::NoDatabasePassword => "no database password",
            UpmError::NoSyncURL => "no sync URL",
            UpmError::NoSyncCredentials => "no sync credentials",
            UpmError::SyncDatabaseNotFound => "remote not found",
            UpmError::Backup(_) => "backup error",
            UpmError::FlatpackOverflow => "flatpack overflow",
            UpmError::DuplicateAccountName(_) => "duplicate account name",
            UpmError::PathNotUnicode(_) => "path is not valid unicode",
        }
    }
    /// For errors which encapsulate another error, allow the caller to fetch the contained error.
    fn cause(&self) -> Option<&error::Error> {
        match *self {
            UpmError::Io(ref err) => Some(err),
            UpmError::Crypto(ref err) => Some(err),
            UpmError::TimeParseError(ref err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for UpmError {
    fn from(err: io::Error) -> UpmError {
        UpmError::Io(err)
    }
}

impl From<openssl::error::ErrorStack> for UpmError {
    fn from(err: openssl::error::ErrorStack) -> UpmError {
        UpmError::Crypto(err)
    }
}

impl From<time::ParseError> for UpmError {
    fn from(err: time::ParseError) -> UpmError {
        UpmError::TimeParseError(err)
    }
}
