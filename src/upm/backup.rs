//! Support for making backups of database file.
//!
//! Because `tupm` is experimental code, we are fairly paranoid about making backups of the
//! database early and often -- perhaps even to the point of annoyance to anyone wondering why
//! their UPM directory is littered with all these files.  Backup databases are suffixed with a
//! timestamp and a `.bak` extension.  Backups are made in the following scenarios:
//!
//! 1. Up to 30 backups of the pre-existing local database are made whenever the database is saved.
//!    If 30 backups are already present, the oldest is deleted to make room for a new one.
//! 2. When a sync operation is about to overwrite a remote database with a new revision, it first
//!    uploads a backup file of the new revision.  If the upload of this backup file fails, the
//!    pre-existing remote database is not deleted and an error is presented to the user.  This is
//!    particularly useful since syncing a new revision consists of non-atomic steps: a "delete"
//!    operation followed by an "upload" operation.  If the "delete" succeeds but the "upload"
//!    fails, the remote database would be lost forever in the absence of backups.  There is
//!    currently no limit on the number of backups stored on the remote server.
//!

use error::UpmError;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use time;

/// The maximum number of backups allowed for the local database.  Old backups will be pruned to
/// keep the number of backups within this limit.
const MAX_BACKUP_FILES: usize = 30;

/// Use this filename extension for backup files.
const BACKUP_FILE_EXTENSION: &'static str = ".bak";

/// Remove the oldest backup files as needed to bring the total number of backup files for this
/// path within the limit.
fn prune_old_backups(path: &Path) -> Result<usize, UpmError> {
    // What is the backup file prefix?
    let prefix = if let Some(s) = path.file_name() {
        match s.to_str() {
            Some(s) => {
                let mut s = String::from(s);
                s.push('.');
                s
            }
            None => return Err(UpmError::InvalidFilename),
        }
    } else {
        return Err(UpmError::InvalidFilename);
    };

    // Build a list of matching files and their modification times
    let mut entries = Vec::<(Box<PathBuf>, SystemTime)>::new();
    for entry in path.canonicalize()?.parent().unwrap().read_dir()? {
        let entry = entry?;
        if let Ok(name) = entry.file_name().into_string() {
            if name.starts_with(&prefix) && name.ends_with(BACKUP_FILE_EXTENSION) {
                let mtime = entry.metadata().unwrap().modified().unwrap();
                entries.push((Box::new(entry.path()), mtime));
            }
        }
    }

    // If too many backup files are present, delete the oldest one(s)
    // to bring us within the limit.
    let mut deletion_count = 0;
    if entries.len() > MAX_BACKUP_FILES {
        entries.sort_by(|a, b| a.1.cmp(&b.1));
        for i in 0..(entries.len() - MAX_BACKUP_FILES) {
            fs::remove_file(entries[i].0.as_path())?;
            deletion_count += 1;
        }
    }
    Ok(deletion_count)
}

/// Generate a backup filename for the specified path by appending a timestamp and `.bak`
/// extension.
pub fn generate_backup_filename<P: AsRef<Path>>(path: P) -> Result<PathBuf, UpmError> {
    let basename = if let Some(s) = path.as_ref().file_name() {
        match s.to_str() {
            Some(x) => x,
            None => return Err(UpmError::InvalidFilename),
        }
    } else {
        return Err(UpmError::InvalidFilename);
    };
    let current_time = time::now();
    let timestamp = match current_time.strftime("%Y%m%d%H%M%S") {
        Ok(t) => t,
        Err(e) => {
            return Err(UpmError::TimeParseError(e));
        }
    };
    let backup_basename = format!("{}.{}{}", basename, timestamp, BACKUP_FILE_EXTENSION);
    Ok(path.as_ref().to_path_buf().with_file_name(backup_basename))
}

/// If the file at the specified path exists, make a backup, and remove any old backup files as
/// needed to bring the total number of backup files for this path within the limit.  `Ok(true)` is
/// returned on success, otherwise an error is returned.
///
/// If the file does not exist, this is not considered an error since it merely means that no
/// backup is needed.  In this case, `Ok(false)` is returned.
pub fn backup(path: &Path) -> Result<bool, UpmError> {
    if !path.exists() {
        // Nothing to backup.
        return Ok(false);
    }

    // Generate the backup filename
    let backup_path = generate_backup_filename(path)?;

    // Make the backup file
    fs::copy(path, backup_path)?;

    // Prune old backups
    // (Ignore errors -- this is best-effort-only.)
    prune_old_backups(path).unwrap_or_default();

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A character-oriented substring function used by our test.
    fn substring(original: &str, position: usize, length: usize) -> &str {
        let start_pos = original
            .char_indices()
            .nth(position)
            .map(|(n, _)| n)
            .unwrap_or(0);
        let end_pos = original
            .char_indices()
            .nth(position + length)
            .map(|(n, _)| n)
            .unwrap_or(0);
        &original[start_pos..end_pos]
    }

    /// Test the generate_backup_filename() function.
    #[test]
    fn test_generate_backup_filename() {
        // Confirm that a bad path returns an error.
        assert_matches!(generate_backup_filename(""), Err(UpmError::InvalidFilename));

        // Confirm that the basic structure of the returned backup path is correct.
        const TEST_PATH: &'static str = "/path/to/file";
        const TIMESTAMP_LENGTH: usize = 14;
        const DECIMAL_RADIX: u32 = 10;
        let test_path_length = TEST_PATH.chars().count();
        let expected_length =
            test_path_length + 1 + TIMESTAMP_LENGTH + BACKUP_FILE_EXTENSION.chars().count();
        let backup_time = time::now();
        let backup_filename = generate_backup_filename(TEST_PATH);
        assert_matches!(backup_filename, Ok(_));
        let backup_filename = backup_filename.unwrap();
        let backup_filename = backup_filename.to_string_lossy();
        assert!(backup_filename.starts_with(TEST_PATH));
        assert!(backup_filename.ends_with(BACKUP_FILE_EXTENSION));
        assert_eq!(backup_filename.chars().count(), expected_length);
        assert_matches!(
            backup_filename.chars().nth(TEST_PATH.chars().count()),
            Some('.')
        );

        // Confirm that the timestamp is correctly rendered to represent the time we asked for the
        // backup filename, +/- 10 seconds.
        const ALLOWED_TIMESTAMP_VARIANCE_SECS: i64 = 10;
        let timestamp = substring(&backup_filename, test_path_length + 1, TIMESTAMP_LENGTH);
        assert!(timestamp.chars().all(|c| c.is_digit(DECIMAL_RADIX)));
        let timestamp_time = time::strptime(timestamp, "%Y%m%d%H%M%S");
        assert_matches!(timestamp_time, Ok(_));
        let mut timestamp_time = timestamp_time.unwrap();
        // The timestamp is parsed as UTC, so force it to be in the correct zone/DST configuration.
        timestamp_time.tm_utcoff = backup_time.tm_utcoff;
        timestamp_time.tm_isdst = backup_time.tm_isdst;
        // Confirm that the timestamp roughly represents the expected time.
        let difference = timestamp_time.to_utc() - backup_time.to_utc();
        assert!(difference < time::Duration::seconds(ALLOWED_TIMESTAMP_VARIANCE_SECS));
    }
}
