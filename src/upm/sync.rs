//! This module supports synchronizing a UPM database with a copy on a remote repository.  The
//! remote repository should be an HTTP or HTTPS server supporting the "download", "upload", and
//! "delete" primitives of the UPM sync protocol.

use multipart::client::lazy::Multipart;
use multipart::server::nickel::nickel::hyper::mime;
use reqwest::multipart;
use std::io::Cursor;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::str;
use std::time::Duration;

use backup;
use database::Database;
use error::UpmError;

/// The UPM sync protocol's delete command.  This is appended to the repository URL.
const DELETE_CMD: &'static str = "deletefile.php";
/// The UPM sync protocol's upload command.  This is appended to the repository URL.
const UPLOAD_CMD: &'static str = "upload.php";
/// This field name is used for the database file when uploading.
const UPM_UPLOAD_FIELD_NAME: &'static str = "userfile";
/// Abort the operation if the server doesn't respond for this time interval.
const TIMEOUT_SECS: u64 = 10;

/// The UPM sync protocol returns an HTTP body of "OK" if the request was successful, otherwise it
/// returns one of these error codes: FILE_DOESNT_EXIST, FILE_WASNT_DELETED, FILE_ALREADY_EXISTS,
/// FILE_WASNT_MOVED, FILE_WASNT_UPLOADED
const UPM_SUCCESS: &'static str = "OK";

/// UPM sync protocol responses should never be longer than this size.
const UPM_MAX_RESPONSE_CODE_LENGTH: usize = 64;

impl From<reqwest::Error> for UpmError {
    /// Convert a reqwest error into a `UpmError`.
    fn from(err: reqwest::Error) -> UpmError {
        UpmError::Sync(format!("{}", err))
    }
}

/// A successful sync will result in one of these three conditions.
pub enum SyncResult {
    /// The remote repository's copy of the database was replaced with the local copy.
    RemoteSynced,
    /// The local database was replaced with the remote repository's copy.
    LocalSynced,
    /// Neither the local database nor the remote database was changed, since they were both the
    /// same revision.
    NeitherSynced,
}

/// Provide basic access to the remote repository.
struct Repository {
    url: String,
    http_username: String,
    http_password: String,
    client: reqwest::Client,
}

impl Repository {
    /// Create a new `Repository` struct with the provided URL and credentials.
    fn new(url: &str, http_username: &str, http_password: &str) -> Repository {
        // Create a new reqwest client.
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .build()
        {
            Ok(cl) => cl,
            Err(e) => {
                let s = format!("Error creating a client: {}", e);
                eprintln!("{}", s);
                std::process::exit(1);
            }
        };

        Repository {
            url: String::from(url),
            http_username: String::from(http_username),
            http_password: String::from(http_password),
            client,
        }
    }

    //
    // Provide the three operations of the UPM sync protocol:
    // Download, delete, and upload.
    //

    /// Download the remote database with the provided name.  The database is returned in raw form
    /// as a byte buffer.
    fn download(&mut self, database_name: &str) -> Result<Vec<u8>, UpmError> {
        let url = self.make_url(database_name);

        // Send request
        let mut response = self
            .client
            .get(&url)
            .basic_auth(self.http_username.clone(), Some(self.http_password.clone()))
            .send()?;

        // Process response
        if !response.status().is_success() {
            return match response.status() {
                reqwest::StatusCode::NOT_FOUND => Err(UpmError::SyncDatabaseNotFound),
                _ => Err(UpmError::Sync(format!("{}", response.status()))),
            };
        }
        let mut data: Vec<u8> = Vec::new();
        response.read_to_end(&mut data)?;
        Ok(data)
    }

    /// Delete the specified database from the remote repository.
    fn delete(&mut self, database_name: &str) -> Result<(), UpmError> {
        let url = self.make_url(DELETE_CMD);

        // Send request
        let mut response = self
            .client
            .post(&url)
            .basic_auth(self.http_username.clone(), Some(self.http_password.clone()))
            .form(&[("fileToDelete", database_name)])
            .send()?;

        // Process response
        self.check_response(&mut response)?;
        Ok(())
    }

    /// Upload the provided database to the remote repository.  The database is provided in raw
    /// form as a byte buffer.
    fn upload(&mut self, database_name: &str, database_bytes: Vec<u8>) -> Result<(), UpmError> {
        let url: String = self.make_url(UPLOAD_CMD);

        // Construct a multipart body
        let mut multipart = Multipart::new();
        let content_type = mime::Mime(
            mime::TopLevel::Application,
            mime::SubLevel::OctetStream,
            vec![],
        );
        multipart.add_stream(
            UPM_UPLOAD_FIELD_NAME,
            Cursor::new(&database_bytes[..]),
            Some(database_name),
            Some(content_type),
        );
        let mut multipart_prepared = match multipart.prepare() {
            Ok(p) => p,
            Err(_) => return Err(UpmError::Sync(String::from("Cannot prepare file upload"))),
        };
        let mut multipart_buffer: Vec<u8> = vec![];
        multipart_prepared.read_to_end(&mut multipart_buffer)?;

        // Thanks to Sean (seanmonstar) for helping to translate this code to multipart code
        // of reqwest
        let dbname = database_name.to_string();
        let part = multipart::Part::bytes(database_bytes.clone())
            .file_name(dbname)
            .mime_str("application/octet-stream")?;

        let form = multipart::Form::new().part(UPM_UPLOAD_FIELD_NAME, part);

        // Send request
        let mut response = self.client.post(&url).multipart(form).send()?;

        // Process response
        self.check_response(&mut response)?;
        Ok(())
    }

    /// Construct a URL by appending the provided string to the repository URL, adding a separating
    /// slash character if needed.
    fn make_url(&self, path_component: &str) -> String {
        if self.url.ends_with('/') {
            format!("{}{}", self.url, path_component)
        } else {
            format!("{}/{}", self.url, path_component)
        }
    }

    /// Confirm that the HTTP response was successful and valid.
    fn check_response(&self, response: &mut reqwest::Response) -> Result<(), UpmError> {
        if !response.status().is_success() {
            return Err(UpmError::Sync(format!("{}", response.status())));
        }
        let mut response_code = String::new();
        response.read_to_string(&mut response_code)?;
        if response_code.len() > UPM_MAX_RESPONSE_CODE_LENGTH {
            return Err(UpmError::Sync(format!(
                "Unexpected response from server ({} bytes)",
                response_code.len()
            )));
        }
        if response_code != UPM_SUCCESS {
            return Err(UpmError::Sync(format!("Server error: {}", response_code)));
        }
        Ok(())
    }
}

/// Download a database from the remote repository without performing any sync operation with a
/// local database.  This is useful when downloading an existing remote database for the first
/// time.
pub fn download<P: AsRef<Path>>(
    repo_url: &str,
    repo_username: &str,
    repo_password: &str,
    database_filename: P,
) -> Result<Vec<u8>, UpmError> {
    let mut repo = Repository::new(repo_url, repo_username, repo_password);
    let name = Database::path_to_name(&database_filename)?;
    repo.download(&name)
}

/// Synchronize the local and remote databases using the UPM sync protocol.  If an optional remote
/// password is provided, it will be used when decrypting the remote database; otherwise, the
/// password of the local database will be used.  Return true if the caller needs to reload the
/// local database.
///
/// The sync logic is as follows:
///
/// 1. Download the current remote database from the provided URL.
///      - Attempt to decrypt this database with the master password.
///      - If decryption fails, return
///      [`UpmError::BadPassword`](../error/enum.UpmError.html#variant.BadPassword).  (The caller
///      may wish to prompt the user for the remote password, then try again.)
/// 2. Take action based on the revisions of the local and remote database:
///      - If the local revision is greater than the remote revision, upload the local database to
///      the remote repository (overwriting the pre-existing remote database).
///      - If the local revision is less than the remote revision, replace the local database with
///      the remote database (overwriting the pre-existing local database).
///      - If the local revision is the same as the remote revision, then do nothing.
/// 3. The caller may wish to mimic the behavior of the UPM Java application by considering the
///    local database to be dirty if it has not been synced in 5 minutes.
///
/// NOTE: It is theoretically possible for two UPM clients to revision the database separately
/// before syncing, and result in a situation where one will "win" and the other will have its
/// changes silently lost.  The caller should exercise the appropriate level of paranoia to
/// mitigate this risk.  For example, prompting for sync before the user begins making a
/// modification, and marking the database as dirty after 5 minutes.
pub fn sync(database: &Database, remote_password: Option<&str>) -> Result<SyncResult, UpmError> {
    // Collect all the facts.
    if database.sync_url.is_empty() {
        return Err(UpmError::NoSyncURL);
    }
    if database.sync_credentials.is_empty() {
        return Err(UpmError::NoSyncCredentials);
    }
    let sync_account = match database.account(&database.sync_credentials) {
        Some(a) => a,
        None => return Err(UpmError::NoSyncCredentials),
    };
    let database_filename = match database.path() {
        Some(f) => f,
        None => return Err(UpmError::NoDatabaseFilename),
    };
    let database_name = match database.name() {
        Some(n) => n,
        None => return Err(UpmError::NoDatabaseFilename),
    };

    let local_password = match database.password() {
        Some(p) => p,
        None => return Err(UpmError::NoDatabasePassword),
    };
    let remote_password = match remote_password {
        Some(p) => p,
        None => local_password,
    };

    // 1. Download the remote database.
    // If the remote database has a different password than the local
    // database, we will return UpmError::BadPassword and the caller can
    // prompt the user for the remote password, and call this function
    // again with Some(remote_password).
    let mut repo = Repository::new(
        &database.sync_url,
        &sync_account.user,
        &sync_account.password,
    );
    let remote_exists;
    let mut remote_database = match repo.download(database_name) {
        Ok(bytes) => {
            remote_exists = true;
            Database::load_from_bytes(&bytes, remote_password)?
        }
        Err(UpmError::SyncDatabaseNotFound) => {
            // No remote database with that name exists, so this must be a fresh sync.
            // We'll use a stub database with revision 0.
            remote_exists = false;
            Database::new()
        }
        Err(e) => return Err(e),
    };

    // 2. Copy databases as needed.
    if database.sync_revision > remote_database.sync_revision {
        // Copy the local database to the remote.

        // First, upload a backup copy in case something goes wrong between delete() and upload().
        if super::PARANOID_BACKUPS {
            let backup_database_path =
                backup::generate_backup_filename(&PathBuf::from(database_name))?;
            let backup_database_name = backup_database_path.to_str();
            if let Some(backup_database_name) = backup_database_name {
                repo.upload(
                    backup_database_name,
                    database.save_to_bytes(remote_password)?,
                )?;
            }
        }

        // Delete the existing remote database, if it exists.
        if remote_exists {
            repo.delete(&database_name)?;
        }

        // Upload the local database to the remote.  Make sure to re-encrypt with the local
        // password, in case it has been changed recently.
        repo.upload(database_name, database.save_to_bytes(local_password)?)?;
        Ok(SyncResult::RemoteSynced)
    } else if database.sync_revision < remote_database.sync_revision {
        // Replace the local database with the remote database
        remote_database.set_path(&database_filename)?;
        remote_database.save()?;
        // The caller should reload the local database when it receives this result.
        Ok(SyncResult::LocalSynced)
    } else {
        // Revisions are the same -- do nothing.
        Ok(SyncResult::NeitherSynced)
    }
}
