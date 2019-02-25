//! Terminal Universal Password Manager
//!
//! This is a terminal implementation of Universal Password Manager, allowing management of UPM
//! databases and synchronization with remote HTTP repositories.

extern crate chrono;
extern crate clap;
#[macro_use(wrap_impl)]
extern crate cursive;
extern crate base64;
extern crate dirs;
extern crate rpassword;
extern crate upm;

use chrono::prelude::*;
use clap::{App, Arg, ArgMatches};
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use tupm::controller::Controller;
use upm::database::Database;
use upm::error::UpmError;
use upm::sync;

mod tupm {
    pub mod clipboard;
    pub mod controller;
    pub mod ui;
}

static DEFAULT_DATABASE_DIRECTORY: &'static str = ".tupm";
static DEFAULT_DATABASE_FILENAME: &'static str = "primary";

// Possible exit codes
static EXIT_SUCCESS: i32 = 0;
static EXIT_FAILURE: i32 = 1;

// These functions supply optional fixed values for the database filename and password if built
// with the test_database feature flag and invoked with the --test option.  This is a convenience
// for development.

#[cfg(feature = "test_database")]
fn test_filename(matches: &ArgMatches) -> Option<PathBuf> {
    static TEST_DB_FILE: &'static str = "sampledb.upm";
    if matches.is_present("test") {
        Some(PathBuf::from(TEST_DB_FILE))
    } else {
        None
    }
}
#[cfg(not(feature = "test_database"))]
fn test_filename(_: &ArgMatches) -> Option<PathBuf> {
    None
}
#[cfg(feature = "test_database")]
fn test_password(matches: &ArgMatches) -> Option<&'static str> {
    static TEST_PASSWORD: &'static str = "my!awesome!password@42";
    if matches.is_present("test") {
        Some(TEST_PASSWORD)
    } else {
        None
    }
}
#[cfg(not(feature = "test_database"))]
fn test_password(_: &ArgMatches) -> Option<&'static str> {
    None
}

/// Return the path to the default database (~/.tupm/primary), creating any intermediate
/// directories if needed.  The actual database file returned by this function may or may not exist
/// yet.
fn get_default_database_path() -> Result<PathBuf, UpmError> {
    // Expand ~/.tupm based on the HOME environment variable
    let mut path = match dirs::home_dir().map(|p| p.join(DEFAULT_DATABASE_DIRECTORY)) {
        Some(d) => d,
        None => return Err(UpmError::InvalidFilename),
    };
    // Create the directory if it doesn't already exist.
    if !path.is_dir() {
        fs::create_dir_all(&path)?;
    }
    // Append the default database filename
    path.push(DEFAULT_DATABASE_FILENAME);
    Ok(path)
}

/// Open the database file at the specified path using the provided password.  Print an error and
/// exit if it cannot be opened, read, and decrypted.
fn open_database_or_exit(filename: &PathBuf, password: &str) -> Database {
    match Database::load_from_file(filename, password) {
        Ok(database) => database,
        Err(e) => {
            println!("error opening database: {}", e);
            process::exit(EXIT_FAILURE);
        }
    }
}

/// Export the contents of the provided database as a text report on standard output.
fn export(database: &Database) {
    // Sort accounts by name.
    let mut accounts = database.accounts.clone();
    accounts.sort();

    // Output current time and database metadata
    println!("# {}", Local::now().format("%a %b %d %T %Y %Z"));
    println!(
        "# revision={} url={} credentials={}",
        database.sync_revision, database.sync_url, database.sync_credentials
    );

    // Short-form output
    macro_rules! exportfmt {
        () => {
            "{:-28} {:-35} {:-10}"
        };
    };
    println!(exportfmt!(), "account", "username", "password");
    println!(
        exportfmt!(),
        "-------------------", "----------------------------------", "------------"
    );
    for account in accounts.iter() {
        println!(exportfmt!(), account.name, account.user, account.password);
    }

    // Long-form output
    println!();
    println!("Long-form output (including URLs and notes)");
    println!("-------------------------------------------");
    println!();
    for account in accounts.iter() {
        // format notes
        let mut notes = account.notes.trim().to_string();
        notes = notes.replace("\r\n", "\n");
        notes = notes.replace("\n", "\n          ");

        println!("Account:  {}", account.name);
        println!("Username: {}", account.user);
        println!("Password: {}", account.password);
        println!("URL:      {}", account.url);
        println!("Notes:    {}", notes);
        println!();
    }
}

/// Download a remote database and exit.  This is useful for fetching a remote database for the
/// first time.
fn download(path: &Path, url: &str) {
    // Avoid overwriting any existing local database -- the user must manually remove the database
    // or specify an alternate path.
    if path.exists() {
        println!(
            "Error: This database already exists: {}",
            path.to_string_lossy()
        );
        println!("(Delete this database or specify an alternate path with --database.)");
        process::exit(EXIT_FAILURE);
    }

    let database_name = match Database::path_to_name(&path) {
        Ok(n) => n,
        Err(e) => {
            println!("Error: {}", e);
            process::exit(EXIT_FAILURE);
        }
    };
    println!(
        "Downloading remote database \"{}\" from repository \"{}\".",
        database_name, url
    );

    // Collect the repository credentials
    let username = rpassword::prompt_response_stdout("Repository username: ").unwrap_or_else(|e| {
        println!("Error reading username: {}", e);
        process::exit(EXIT_FAILURE);
    });
    let password = rpassword::prompt_password_stdout("Repository password: ").unwrap_or_else(|e| {
        println!("Error reading password: {}", e);
        process::exit(EXIT_FAILURE);
    });

    // Download
    let database_bytes = match sync::download(url, &username, &password, path) {
        Ok(d) => d,
        Err(e) => {
            println!("Error downloading database: {}", e);
            process::exit(EXIT_FAILURE);
        }
    };
    println!("{} bytes downloaded from repository.", database_bytes.len());

    // Save
    if let Err(e) = Database::save_raw_bytes(database_bytes, path) {
        println!("Error saving database: {}", e);
        process::exit(EXIT_FAILURE);
    }
    println!("Database written to: {}.", path.to_string_lossy());
}

/// Parse the command-line arguments and present a user interface with the selected UPM database.
fn main() {
    // Parse command-line arguments
    let app = App::new("Terminal Universal Password Manager")
        .version("0.1.0")
        .about("Provides a terminal interface to Universal Password Manager (UPM) databases.")
        .arg(
            Arg::with_name("database")
                .short("d")
                .long("database")
                .value_name("FILE")
                .help("Specify the path to the database.")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("password")
                .short("p")
                .long("password")
                .help("Prompt for a password."),
        )
        .arg(
            Arg::with_name("export")
                .short("e")
                .long("export")
                .help("Export database to a flat text file."),
        )
        .arg(
            Arg::with_name("download")
                .short("l")
                .long("download")
                .value_name("URL")
                .help("Download a remote database.")
                .takes_value(true),
        );
    #[cfg(feature = "test_database")]
    let app = app.arg(
        Arg::with_name("test")
            .short("t")
            .long("test")
            .help("Loads ./sampledb.upm with a baked-in password."),
    );
    let matches = app.get_matches();

    // Determine the database path.
    let database_filename = matches
        .value_of("database")
        .map(|p| PathBuf::from(p))
        .or(test_filename(&matches))
        .unwrap_or(get_default_database_path().unwrap_or_else(|e| {
            println!("Error resolving default database path: {}", e);
            process::exit(EXIT_FAILURE);
        }));

    // Determine the database password, if possible
    let password = if matches.is_present("password") {
        Some(
            rpassword::prompt_password_stdout("Password: ").unwrap_or_else(|e| {
                println!("Error reading password: {}", e);
                process::exit(EXIT_FAILURE);
            }),
        )
    } else {
        test_password(&matches).map(|p| String::from(p))
    };

    // Dispatch to non-UI tasks, if requested.
    if matches.is_present("export") {
        match password {
            Some(p) => export(&open_database_or_exit(&database_filename, p.as_str())),
            None => {
                println!("Cannot export without a password.  Use --password to prompt.");
                process::exit(EXIT_FAILURE);
            }
        }
        process::exit(EXIT_SUCCESS);
    }
    if let Some(url) = matches.value_of("download") {
        download(&database_filename, url);
        process::exit(EXIT_SUCCESS);
    }

    // Launch the controller and UI.
    let controller = Controller::new(&database_filename, password);
    match controller {
        Ok(mut controller) => controller.run(),
        Err(e) => {
            println!("Error: {}", e);
            process::exit(EXIT_FAILURE);
        }
    }
}
