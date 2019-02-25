//! Read and write Universal Password Manager version 3 databases.  This code is meant to
//! interoperate with the format used by [the original UPM Java
//! application](https://github.com/adrian/upm-swing).
//!
//! Versions 1 and 2 of the UPM database format are not supported.  Version 3 was introduced in
//! 2011, so there may not be many cases where the older versions are still in use.
//!
//! # Database format
//!
//! UPMv3 databases are stored in the following format:
//!
//! * A 3-byte magic field ("UPM").
//! * A 1-byte version field.  (This module only supports version 3.)
//! * The 8-byte salt used to encrypt the remainder of the file.
//! * The remainder of the file is encrypted using 256-bit AES-CBC (see the documentation for the
//!   [`crypto`] module for more details).  When decrypted, the plaintext will contain a series of
//!   length-prefixed records in a format that the original UPM author refers to as "flatpack".
//!   The length prefix is four bytes of UTF-8 encoded decimal which specifies the size in bytes of
//!   the record payload which follows.  The payload is always a UTF-8 string; integers are encoded
//!   as decimal digits.
//!    - The first three records are metadata:
//!        1. The database revision, a monotonically increasing number that is used when syncing
//!        with a remote database.
//!        2. The URL of the remote sync repository.  This URL does not include the name of the
//!           database.  It instead corresponds to a directory on the server which may include
//!           multiple UPM databases with different names.
//!        3. The name of the account, as included in this database, which contains the username
//!           and password to be used for HTTP Basic Authentication when accessing the remote sync
//!           repository.
//!     - The remaining records contain account data.  Every five records represents the following
//!       data for a specific account:
//!        1. Account name
//!        2. Username
//!        3. Password
//!        4. URL
//!        5. Notes

use crypto;
use error::UpmError;
use rand::{OsRng, Rng};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::fs::File;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str;
use std::time::Duration;
use std::time::Instant;

/// The size in bytes of the UPM header magic field.
const MAGIC_SIZE: usize = 3;
/// The expected magic.
const UPM_MAGIC: [u8; MAGIC_SIZE] = ['U' as u8, 'P' as u8, 'M' as u8];
/// The size in bytes of the UPM header version field.
const UPM_DB_VERSION_SIZE: usize = 1;
/// The expected database version.
const UPM_DB_VERSION: u8 = 3;
/// The size in bytes of the header salt field.
const SALT_SIZE: usize = 8;

/// After this much time elapses from the last synch, the database will once again be considered
/// unsynced (i.e. dirty).  This mimics the behavior of the java-swing UPM client.
const SYNC_VALIDITY_SECS: u64 = 300; // 5 minutes

/// A flatpack record cannot contain more than 9999 bytes.
const FLATPACK_MAX_RECORD_SIZE: usize = 9999;

/// This struct provides a means of consuming flatpack records from a binary buffer.
///
/// Flatpack data contains a series of length-prefixed records.  The length prefix is four bytes of
/// UTF-8 encoded decimal which specifies the size in bytes of the record payload which follows.
/// The payload is always a UTF-8 string; integers are encoded as decimal digits.
struct FlatpackParser {
    buffer: Vec<u8>,
    position: usize,
    error: bool,
}

impl<'a> Iterator for FlatpackParser {
    type Item = Result<String, UpmError>;

    /// Return the next record, if present.  Return `None` when the end of iteration is reached.
    fn next(&mut self) -> Option<Result<String, UpmError>> {
        fn make_error(message: &str) -> Option<Result<String, UpmError>> {
            Some(Err(UpmError::AccountParse(Some(String::from(message)))))
        }

        // Handle exceptional conditions.
        if self.error {
            return None;
        }
        if self.position == self.buffer.len() {
            return None;
        }
        if self.position > self.buffer.len() - 4 {
            self.error = true;
            return make_error("buffer underrun while parsing length prefix");
        }

        // Extract the length prefix.
        let mut size: usize = 0;
        for i in 0..4 {
            let c = self.buffer[self.position + i];
            if c < '0' as u8 || c > '9' as u8 {
                self.error = true;
                return make_error("invalid byte in length prefix");
            }
            size += ((c - ('0' as u8)) as usize) * 10usize.pow(3 - (i as u32));
        }
        self.position += 4;

        // Extract the payload
        if self.position + size > self.buffer.len() {
            self.error = true;
            return make_error("buffer underrun while parsing payload");
        }
        let payload_bytes = &self.buffer[self.position..self.position + size];
        let payload = match str::from_utf8(payload_bytes) {
            Ok(s) => String::from(s),
            Err(e) => return Some(Err(UpmError::AccountParse(Some(format!("{}", e))))),
        };
        self.position += size;

        Some(Ok(payload))
    }
}

impl FlatpackParser {
    /// Construct a new flatpack parser with the provided byte buffer.
    fn new(buffer: Vec<u8>) -> FlatpackParser {
        FlatpackParser {
            buffer: buffer,
            position: 0,
            error: false,
        }
    }

    /// Parse and return the next `count` records.
    fn get(&mut self, count: usize) -> Result<Vec<String>, UpmError> {
        let mut items: Vec<String> = Vec::new();
        for _ in 0..count {
            items.push(match self.next() {
                Some(Ok(s)) => s,
                Some(Err(e)) => return Err(e),
                None => {
                    return Err(UpmError::AccountParse(Some(String::from(
                        "record underrun",
                    ))));
                }
            });
        }
        return Ok(items);
    }

    /// Return true if the parser has reached the end of the flatpack data.
    fn eof(&self) -> bool {
        self.position == self.buffer.len()
    }

    /// Convenience function to return a 3-tuple of the next three records.
    fn take3(&mut self) -> Result<(String, String, String), UpmError> {
        let mut v = self.get(3)?;
        Ok((v.remove(0), v.remove(0), v.remove(0)))
    }

    /// Convenience function to return a 5-tuple of the next five records.
    fn take5(&mut self) -> Result<(String, String, String, String, String), UpmError> {
        let mut v = self.get(5)?;
        Ok((
            v.remove(0),
            v.remove(0),
            v.remove(0),
            v.remove(0),
            v.remove(0),
        ))
    }
}

/// This struct provides a means of encoding data as flatpack records.
struct FlatpackWriter {
    buffer: Vec<u8>,
}

impl FlatpackWriter {
    /// Construct a new flatpack writer.
    fn new() -> FlatpackWriter {
        FlatpackWriter {
            buffer: Vec::<u8>::new(),
        }
    }

    /// Add a record containing the provided bytes.
    fn put_bytes(&mut self, data: &[u8]) -> Result<(), UpmError> {
        // Validate record length
        if data.len() > FLATPACK_MAX_RECORD_SIZE {
            return Err(UpmError::FlatpackOverflow);
        }
        // Write the length prefix
        self.buffer.extend(format!("{:04}", data.len()).as_bytes());
        // Write the data
        self.buffer.extend(data);
        Ok(())
    }

    /// Add a record containing the provided string.
    fn put_string(&mut self, data: &str) -> Result<(), UpmError> {
        self.put_bytes(data.as_bytes())?;
        Ok(())
    }

    /// Add a record containing the provided integer.
    fn put_u32(&mut self, number: u32) -> Result<(), UpmError> {
        self.put_string(&(format!("{}", number)))?;
        Ok(())
    }
}

/// This struct represents a single UPM account, and provides an ordering based on the
/// alphanumeric case-insensitive comparison of account names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub user: String,
    pub password: String,
    pub url: String,
    pub notes: String,
}

impl Account {
    /// Create a new Account struct.  All fields are initialized to empty strings.
    pub fn new() -> Account {
        Account {
            name: String::new(),
            user: String::new(),
            password: String::new(),
            url: String::new(),
            notes: String::new(),
        }
    }
}

impl Ord for Account {
    /// Provide an ordering of accounts based on a case-insensitive comparison of account names.
    fn cmp(&self, other: &Account) -> Ordering {
        self.name.to_lowercase().cmp(&other.name.to_lowercase())
    }
}

impl PartialOrd for Account {
    fn partial_cmp(&self, other: &Account) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// This struct represents a UPM database, as read from a local file or a remote sync repository.
#[derive(Clone)]
pub struct Database {
    pub sync_revision: u32,
    pub sync_url: String,
    pub sync_credentials: String,
    pub accounts: Vec<Account>,
    /// Track the filename originally used to load this file.  This will be used when saving and
    /// syncing with a remote repository.
    path: Option<PathBuf>,
    /// Track the password used to decrypt this database, so it can be used to re-encrypt when
    /// saving and syncing.
    password: Option<String>,
    /// Record the time of last sync.  Some edit features only work when the database has been
    /// recently synced.
    last_synced: Option<Instant>,
}

impl fmt::Debug for Database {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Database[r={},a={}]",
            self.sync_revision,
            self.accounts.len()
        )
    }
}

impl Database {
    /// Construct a fresh, empty database.
    pub fn new() -> Database {
        Database {
            sync_revision: 0,
            sync_url: String::new(),
            sync_credentials: String::new(),
            accounts: vec![],
            path: None,
            password: None,
            last_synced: None,
        }
    }

    /// Load and decrypt a database from an in-memory byte slice using the provided password.
    pub fn load_from_bytes(bytes: &[u8], password: &str) -> Result<Database, UpmError> {
        // Remove a number of bytes from a byte buffer.  Return a tuple containing the removed bytes
        // and the remaining bytes.
        fn unshift(bytes: &[u8], size: usize) -> (&[u8], &[u8]) {
            (&bytes[0..size], &bytes[size..])
        }

        // Parse the unencrypted header
        const HEADER_SIZE: usize = MAGIC_SIZE + UPM_DB_VERSION_SIZE + SALT_SIZE;
        if bytes.len() < HEADER_SIZE {
            return Err(UpmError::ReadUnderrun);
        }
        let (magic, remainder) = unshift(bytes, MAGIC_SIZE);
        if magic != UPM_MAGIC {
            return Err(UpmError::BadMagic);
        }
        let (db_version, remainder) = unshift(remainder, UPM_DB_VERSION_SIZE);
        if db_version[0] != UPM_DB_VERSION {
            return Err(UpmError::BadVersion(db_version[0]));
        }
        let (salt, ciphertext) = unshift(remainder, SALT_SIZE);

        // Decrypt the ciphertext
        let plaintext = crypto::decrypt(&ciphertext, password, &salt)?;

        // The resulting plaintext is encoded as a series of "flatpack" records.
        let mut pack = FlatpackParser::new(plaintext);

        // The initial three elements are metadata.
        let (sync_revision, sync_url, sync_credentials) = pack.take3()?;
        let sync_revision: u32 = match sync_revision.parse() {
            Ok(r) => r,
            Err(_) => {
                return Err(UpmError::AccountParse(Some(String::from(
                    "cannot parse revision number",
                ))));
            }
        };

        // Accounts follow in groups of five elements.
        let mut accounts: Vec<Account> = Vec::new();
        while !pack.eof() {
            let elements = pack.take5()?;
            let record = Account {
                name: elements.0,
                user: elements.1,
                password: elements.2,
                url: elements.3,
                notes: elements.4,
            };
            accounts.push(record);
        }

        // Assure account names are unique when loading, so we can rely on this as a key later.
        let mut account_names = HashSet::new();
        for ref account in &accounts {
            if account_names.contains(&account.name) {
                return Err(UpmError::DuplicateAccountName(account.name.clone()));
            }
            account_names.insert(account.name.clone());
        }

        Ok(Database {
            sync_revision: sync_revision,
            sync_url: sync_url,
            sync_credentials: sync_credentials,
            accounts: accounts,
            path: None,
            password: Some(String::from(password)),
            last_synced: None,
        })
    }

    /// Load and decrypt a database from the given filename using the provided password.
    pub fn load_from_file<P: AsRef<Path>>(
        filename: P,
        password: &str,
    ) -> Result<Database, UpmError> {
        let mut file = File::open(filename.as_ref())?;
        let mut bytes: Vec<u8> = Vec::new();
        file.read_to_end(&mut bytes)?;
        drop(file);
        let mut database = Database::load_from_bytes(&bytes, password)?;
        database.set_path(&filename.as_ref())?;
        Ok(database)
    }

    /// Save the database locally using the same filename previously used to load the database.
    pub fn save(&self) -> Result<(), UpmError> {
        let filename = match self.path() {
            Some(f) => f,
            None => return Err(UpmError::NoDatabaseFilename),
        };
        let password = match self.password() {
            Some(p) => p,
            None => return Err(UpmError::NoDatabasePassword),
        };
        self.save_as(filename, password)
    }

    /// Save the database locally using the provided filename and password.
    pub fn save_as(&self, filename: &Path, password: &str) -> Result<(), UpmError> {
        let bytes = self.save_to_bytes(password)?;
        Self::save_raw_bytes(bytes, filename)?;
        Ok(())
    }

    /// Save an already-encoded database locally using the provided filename.
    pub fn save_raw_bytes<P: AsRef<Path>>(bytes: Vec<u8>, filename: P) -> Result<(), UpmError> {
        // First write to a temporary file, then rename().  This avoids
        // destroying the existing file if an I/O error occurs (e.g. out
        // of space).

        // Determine the temporary filename
        Self::validate_path(&filename)?;
        let filename = filename.as_ref();
        let tmp_filename = PathBuf::from(String::from(filename.to_str().unwrap()) + ".tmp");

        // Remove any existing temporary file, if present.
        match fs::remove_file(&tmp_filename) {
            Ok(_) => {}
            Err(ref e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(UpmError::Io(e)),
        }

        // Write the file.
        {
            // Use a separate lexical scope for the file, so it will be
            // flushed and closed before we rename.  (Renaming an open
            // file probably isn't an issue under Unix, but I'm not
            // sure about other operating systems.)
            let mut file = File::create(&tmp_filename)?;
            file.write_all(&bytes)?;
        }
        // Rename the temporary file to the real filename.
        fs::rename(tmp_filename, filename)?;
        Ok(())
    }

    /// Save the database to an in-memory byte buffer.  This is useful, for example, when sending
    /// the database to a remote sync repository.
    pub fn save_to_bytes(&self, password: &str) -> Result<Vec<u8>, UpmError> {
        let mut buffer: Vec<u8> = vec![];

        // Generate a salt
        let mut rng = OsRng::new().ok().unwrap();
        let mut salt = [0u8; SALT_SIZE];
        rng.fill_bytes(&mut salt);

        // Write unencrypted metadata
        buffer.extend_from_slice(&UPM_MAGIC);
        buffer.extend_from_slice(&[UPM_DB_VERSION]);
        buffer.extend_from_slice(&salt);

        // Write encrypted metadata
        let mut pack = FlatpackWriter::new();
        pack.put_u32(self.sync_revision)?;
        pack.put_string(&self.sync_url)?;
        pack.put_string(&self.sync_credentials)?;

        // Write accounts
        for account in self.accounts.iter() {
            pack.put_string(&account.name)?;
            pack.put_string(&account.user)?;
            pack.put_string(&account.password)?;
            pack.put_string(&account.url)?;
            pack.put_string(&account.notes)?;
        }

        // Encrypt and write to the file
        let ciphertext = crypto::encrypt(&pack.buffer, password, &salt)?;
        buffer.extend_from_slice(ciphertext.as_slice());
        Ok(buffer)
    }

    /// Return a reference to the named account.
    pub fn account(&self, name: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.name == name)
    }

    /// Return a mutable reference to the named account.
    pub fn account_mut(&mut self, name: &str) -> Option<&mut Account> {
        self.accounts.iter_mut().find(|a| a.name == name)
    }

    /// Return true if the database contains an account with the specified name; otherwise return
    /// false.
    pub fn contains(&self, name: &str) -> bool {
        self.accounts.iter().any(|a| a.name == name)
    }

    /// Update the named account with the fields in the provided account object.  The account
    /// object may contain a new account name for this account.
    pub fn update_account(&mut self, name: &str, new_account: &Account) -> Result<(), UpmError> {
        // Check for name collision
        if name != new_account.name && self.contains(&new_account.name) {
            return Err(UpmError::DuplicateAccountName(new_account.name.clone()));
        }

        // Update account
        if let Some(account) = self.account_mut(name) {
            account.name = new_account.name.clone();
            account.user = new_account.user.clone();
            account.password = new_account.password.clone();
            account.url = new_account.url.clone();
            account.notes = new_account.notes.clone();
        }
        Ok(())
    }

    /// Add a copy of the provided account object to the database as a new account.
    pub fn add_account(&mut self, new_account: &Account) -> Result<(), UpmError> {
        // Check for name collision
        if self.contains(&new_account.name) {
            return Err(UpmError::DuplicateAccountName(new_account.name.clone()));
        }

        // Add account
        self.accounts.push(new_account.clone());
        Ok(())
    }

    /// Delete the specified account from the database.
    pub fn delete_account(&mut self, name: &str) {
        self.accounts.retain(|ref a| a.name != name);
    }

    /// Return true if this database has a remote sync repository configured; otherwise return
    /// false.
    pub fn has_remote(&self) -> bool {
        !self.sync_url.is_empty()
    }

    /// Validate that the provided path is valid Unicode and has a final component.  After this
    /// validation, path.to_str().unwrap() and path.file_name().unwrap() may be safely used.
    fn validate_path<P: AsRef<Path>>(path: &P) -> Result<(), UpmError> {
        // Only allow paths that are valid Unicode.  This allows us to safely unwrap() the path's
        // to_str() later, instead of handling a potential encoding issue each time.
        if path.as_ref().to_str().is_none() {
            return Err(UpmError::PathNotUnicode(
                path.as_ref().to_string_lossy().into_owned(),
            ));
        }

        // Only allow paths that contain a final component, which is assumed to be the database
        // file.  This allows us to safely unwrap() the path's file_name() later, instead of
        // handling this error each time.
        if path.as_ref().file_name().is_none() {
            return Err(UpmError::InvalidFilename);
        }
        Ok(())
    }

    /// Set the path of the local database to the specified path.
    pub fn set_path<P: AsRef<Path>>(&mut self, path: &P) -> Result<(), UpmError> {
        Self::validate_path(path)?;
        self.path = Some(path.as_ref().to_path_buf());
        Ok(())
    }

    /// Return the path to the local database, if known.
    pub fn path(&self) -> Option<&Path> {
        match &self.path {
            &Some(ref p) => Some(p.as_path()),
            &None => None,
        }
    }

    /// Return the name of the database, if available.  The name is the final path component of the
    /// database in the local filesystem.
    pub fn name(&self) -> Option<&str> {
        match self.path {
            // These unwrap()'s are safe thanks to validation in set_path().
            Some(ref p) => Some(p.file_name().unwrap().to_str().unwrap()),
            None => None,
        }
    }

    /// Return the name of a database that is represented by the provided filesystem path.
    pub fn path_to_name<P: AsRef<Path>>(path: &P) -> Result<&str, UpmError> {
        Self::validate_path(path)?;
        // These unwrap()'s are safe thanks to validate_path().
        Ok(path.as_ref().file_name().unwrap().to_str().unwrap())
    }

    /// Set the password used to encrypt this database.
    pub fn set_password<P: AsRef<str>>(&mut self, password: &P) {
        self.password = Some(password.as_ref().to_owned());
    }

    /// Retrieve the password used to encrypt and decrypt this database.
    pub fn password(&self) -> Option<&str> {
        match &self.password {
            &Some(ref p) => Some(p.as_str()),
            &None => None,
        }
    }

    /// Mark the database as being synchronized with the remote sync repository.  This is only
    /// valid for 5 minutes.
    pub fn set_synced(&mut self) {
        self.last_synced = Some(Instant::now());
    }

    /// Mark the database as not being synchronized with the remote sync repository.
    pub fn clear_synced(&mut self) {
        self.last_synced = None;
    }

    /// Return true if the database is synchronized with the remote sync repository; otherwise
    /// return false.
    pub fn is_synced(&self) -> bool {
        match self.last_synced {
            Some(t) => t.elapsed() < Duration::from_secs(SYNC_VALIDITY_SECS),
            None => false,
        }
    }
}

impl fmt::Display for Database {
    /// Print basic information about this database.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Database(rev={},url={},cred={},count={})",
            self.sync_revision,
            self.sync_url,
            self.sync_credentials,
            self.accounts.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flatpack() {
        const RECORD_0: &str = "hello";
        const RECORD_1: u32 = 0;
        const RECORD_2: u32 = 0x100;
        const RECORD_3: u32 = 0xFFFFFFFF;
        #[cfg_attr(rustfmt, rustfmt_skip)]
        const RECORD_4: &[u8] = &[
            0xCE, 0xB3, 0xCE, 0xBB, 0xCF, 0x8E, 0xCF, 0x83,
            0xCF, 0x83, 0xCE, 0xB1
        ];
        const RECORD_5: u32 = 0;
        const RECORD_6: u32 = 0x100;
        const RECORD_7: u32 = 0xFFFFFFFF;
        const RECORD_8: &str = "goodbye";

        // Test flatpack encoding
        let mut flatpack = FlatpackWriter::new();
        flatpack.put_string(RECORD_0).unwrap();
        flatpack.put_u32(RECORD_1).unwrap();
        flatpack.put_u32(RECORD_2).unwrap();
        flatpack.put_u32(RECORD_3).unwrap();
        flatpack.put_bytes(RECORD_4).unwrap();
        flatpack.put_u32(RECORD_5).unwrap();
        flatpack.put_u32(RECORD_6).unwrap();
        flatpack.put_u32(RECORD_7).unwrap();
        flatpack.put_string(RECORD_8).unwrap();
        let buffer = flatpack.buffer;

        // Test flatpack decoding
        let mut parser = FlatpackParser::new(buffer);
        assert!(parser.eof() == false);
        assert_matches!(parser.next(), Some(Ok(ref s)) if s == RECORD_0 );
        assert!(parser.eof() == false);
        assert_matches!(parser.take3(),
        Ok((ref a, ref b, ref c)) if
            *a == format!("{}", RECORD_1) &&
            *b == format!("{}", RECORD_2) &&
            *c == format!("{}", RECORD_3)
        );
        assert!(parser.eof() == false);
        assert_matches!(parser.take5(),
        Ok((ref a, ref b, ref c, ref d, ref e)) if
            (*a).as_bytes() == RECORD_4 &&
            *b == format!("{}", RECORD_5) &&
            *c == format!("{}", RECORD_6) &&
            *d == format!("{}", RECORD_7) &&
            *e == RECORD_8
        );
        assert!(parser.eof());
    }

    #[test]
    fn test_account_ordering() {
        const UNORDERED_NAMES: [&str; 5] = ["Marlin", "zebra", "Aardvark", "lark", "tiger"];
        const ORDERED_NAMES: [&str; 5] = ["Aardvark", "lark", "Marlin", "tiger", "zebra"];
        let mut accounts: Vec<Account> = vec![];
        for name in UNORDERED_NAMES.iter() {
            accounts.push(Account {
                name: String::from(*name),
                user: String::from("user"),
                password: String::from("password"),
                url: String::from("url"),
                notes: String::from("notes"),
            });
        }
        accounts.sort();
        let names: Vec<String> = accounts.iter().map(|a| a.name.clone()).collect();
        assert_eq!(names.as_slice(), ORDERED_NAMES);
    }

    const PASSWORD: &str = "xyzzy";
    const INCORRECT_PASSWORD: &str = "frobozz";

    /// This is a small database encrypted with the above password.
    #[cfg_attr(rustfmt, rustfmt_skip)]
    const DATABASE_BYTES: &[u8] = &[
        0x55, 0x50, 0x4D, 0x03, 0x35, 0xB3, 0x66, 0xE2,
        0xF5, 0x28, 0xBF, 0x3E, 0x0E, 0xF5, 0x4D, 0xD8,
        0x47, 0x6B, 0xC2, 0x4E, 0xA0, 0xA0, 0x47, 0x02,
        0x20, 0x25, 0xD8, 0xDB, 0x01, 0x41, 0xB2, 0x06,
        0xE2, 0xB1, 0x50, 0x93, 0xC1, 0x26, 0x01, 0xE9,
        0xA0, 0x96, 0xFA, 0xC7, 0x0B, 0xE7, 0x80, 0x4F,
        0x05, 0x4E, 0xE7, 0x76, 0x4F, 0xC3, 0x42, 0xAC,
        0x76, 0x81, 0x27, 0x8B
    ];

    fn assert_accounts(database: &Database, expected_accounts: &[&str]) {
        let mut accounts: Vec<String> = database.accounts.iter().map(|a| a.name.clone()).collect();
        accounts.sort();
        let mut expected_accounts: Vec<&str> = expected_accounts.to_vec();
        expected_accounts.sort();
        if accounts != expected_accounts {
            panic!("expected: {:?} received: {:?}", expected_accounts, accounts);
        }
    }

    #[test]
    fn test_database() {
        // Load a database with an incorrect password
        let result = Database::load_from_bytes(DATABASE_BYTES, INCORRECT_PASSWORD);
        assert_matches!(result, Err(_));

        // Load a database
        let result = Database::load_from_bytes(DATABASE_BYTES, PASSWORD);
        assert_matches!(result, Ok(_));
        let mut database = result.unwrap();

        // Verify data
        assert_eq!(database.sync_revision, 1);
        assert_matches!(database.password, Some(ref p) if p == PASSWORD);
        assert_eq!(database.accounts.len(), 1);
        assert_eq!(database.accounts[0].name, "acct");
        assert_eq!(database.accounts[0].user, "user");
        assert_eq!(database.accounts[0].password, "pass");

        // Test account()/account_mut()/contains()
        assert_matches!(database.account("noacct"), None);
        assert_matches!(database.account_mut("noacct"), None);
        assert_matches!(database.account("acct"), Some(_));
        assert_matches!(database.account_mut("acct"), Some(_));
        assert_eq!(database.account("acct").unwrap().name, "acct");
        assert_eq!(database.account_mut("acct").unwrap().name, "acct");
        assert!(database.contains("acct"));
        assert!(database.contains("noacct") == false);

        // Add, modify, and delete accounts.
        assert_accounts(&database, &["acct"]);
        let result = database.add_account(&Account {
            name: String::from("acct2"),
            user: String::from("user2"),
            password: String::from("pass2"),
            url: String::from(""),
            notes: String::from(""),
        });
        assert_matches!(result, Ok(()));
        assert_accounts(&database, &["acct", "acct2"]);
        let result = database.add_account(&Account {
            name: String::from("acct3"),
            user: String::from("user3"),
            password: String::from("pass3"),
            url: String::from(""),
            notes: String::from(""),
        });
        assert_matches!(result, Ok(()));
        assert_accounts(&database, &["acct", "acct2", "acct3"]);
        let result = database.update_account(
            "acct",
            &Account {
                name: String::from("acct1"),
                user: String::from("user1"),
                password: String::from("pass1"),
                url: String::from(""),
                notes: String::from(""),
            },
        );
        assert_matches!(result, Ok(()));
        assert_accounts(&database, &["acct1", "acct2", "acct3"]);
        database.delete_account("acct2");
        assert_accounts(&database, &["acct1", "acct3"]);

        // Confirm that duplicate account names cannot be created.
        let result = database.update_account(
            "acct1",
            &Account {
                name: String::from("acct3"),
                user: String::from("user1"),
                password: String::from("pass1"),
                url: String::from(""),
                notes: String::from(""),
            },
        );
        assert_matches!(result, Err(UpmError::DuplicateAccountName(ref n)) if n == "acct3");
        let result = database.add_account(&Account {
            name: String::from("acct1"),
            user: String::from("user1"),
            password: String::from("pass1"),
            url: String::from(""),
            notes: String::from(""),
        });
        assert_matches!(result, Err(UpmError::DuplicateAccountName(ref n)) if n == "acct1");

        // Save the database
        let result = database.save_to_bytes(PASSWORD);
        assert_matches!(result, Ok(_));
        let bytes = result.unwrap();

        // Re-load the database
        let result = Database::load_from_bytes(&bytes, INCORRECT_PASSWORD);
        assert_matches!(result, Err(_));
        let result = Database::load_from_bytes(&bytes, PASSWORD);
        assert_matches!(result, Ok(_));
        let database = result.unwrap();

        // Verify data
        assert_accounts(&database, &["acct1", "acct3"]);
        assert_eq!(database.account("acct1").unwrap().user, "user1");
        assert_eq!(database.account("acct1").unwrap().password, "pass1");
        assert_eq!(database.account("acct3").unwrap().user, "user3");
        assert_eq!(database.account("acct3").unwrap().password, "pass3");
    }

    #[cfg_attr(rustfmt, rustfmt_skip)]
    const VALID_UTF8: &[u8] = &[
        0xCE, 0xB3, 0xCE, 0xBB, 0xCF, 0x8E, 0xCF, 0x83,
        0xCF, 0x83, 0xCE, 0xB1
    ];

    #[test]
    fn test_validate_path() {
        assert_matches!(Database::validate_path(&""), Err(UpmError::InvalidFilename));
        assert_matches!(Database::validate_path(&"file"), Ok(()));
        assert_matches!(Database::validate_path(&"/path/to/file"), Ok(()));
        assert_matches!(Database::validate_path(&"/path/to/dir/"), Ok(()));
        assert_matches!(
            Database::validate_path(&PathBuf::from(
                String::from_utf8(VALID_UTF8.to_vec()).unwrap()
            )),
            Ok(())
        );
        // It's not obvious how to test paths with invalid Unicode encodings to make sure they
        // result in UpmError::PathNotUnicode.  Such a test would likely work differently on
        // different platforms, due to differences in the OsStr implementations.
    }
}
