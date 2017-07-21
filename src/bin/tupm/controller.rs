//! Provide the core logic of the `tupm` application: receive events from the UI and perform
//! operations on the currently loaded database.
//!

extern crate upm;

use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use upm::backup::backup;
use upm::database::{Database, Account};
use upm::error::UpmError;
use upm::sync;
use upm::sync::SyncResult;
use tupm;

/// The controller maintains a message queue consisting of zero or more of these messages.  Other
/// components (mostly likely the UI) can add messages to the queue, and the controller will
/// process them in order.
#[derive(Debug)]
pub enum Message {
    AccountEdit(Option<Account>, Option<Account>),
    DatabaseEdit(String, String),
    Sync,
    ChangePassword(String),
    Quit,
}

/// This struct provides the core logic of the `tupm` application.  It fulfills the role of the
/// controller in the Model-View-Controller (MVC) design pattern.  (The Ui class provides the view,
/// and the Database class provides the model.)
pub struct Controller {
    rx: mpsc::Receiver<Message>,
    ui: tupm::ui::Ui,
    database: Database,
}

impl Controller {
    /// Create a new controller with the provided database path and password.  This will load the
    /// database (if possible) and initialize the user interface.
    pub fn new(database_path: &PathBuf, password: Option<String>) -> Result<Controller, UpmError> {
        let (tx, rx) = mpsc::channel::<Message>();
        let mut ui = tupm::ui::Ui::new(tx.clone());
        let mut fresh_database = false;
        let mut database_try: Option<Database>;
        let mut database;
        let mut retry;
        let mut subsequent_bad_password = false;

        // Prompt for a password if none was supplied.
        let mut password = match password {
            Some(p) => p,
            None => {
                subsequent_bad_password = true;
                match Controller::password_prompt(&mut ui) {
                    Some(p) => p,
                    None => return Err(UpmError::NoDatabasePassword),
                }
            }
        };

        // This awkward syntax is how a do-while is implemented in Rust.
        while {
            retry = false;
            database_try = match Database::load_from_file(database_path, &password) {
                Ok(mut database) => {
                    database.accounts.sort();
                    ui.set_statusline(&format!("Database loaded from {}", database_path.display()));
                    Some(database)
                }
                Err(UpmError::Io(ref e)) if e.kind() == io::ErrorKind::NotFound => {
                    ui.set_statusline("No existing database found -- creating a new database.");
                    fresh_database = true;
                    None
                }
                Err(UpmError::BadPassword) => {
                    if subsequent_bad_password {
                        ui.notice_dialog(
                            "Bad password",
                            "The provided password is invalid for this database.",
                        );
                    } else {
                        subsequent_bad_password = true;
                    }
                    match Controller::password_prompt(&mut ui) {
                        Some(p) => {
                            password = p;
                            retry = true;
                        }
                        None => return Err(UpmError::NoDatabasePassword),
                    };
                    None
                }
                Err(e) => {
                    ui.notice_dialog(
                        "Unrecoverable error",
                        &format!(
                            "The database could not be opened for the \
                            following reason:\n\n{}\n\nThe program will \
                            now exit.",
                            e
                        ),
                    );
                    ui.quit();
                    None
                }
            };
            retry
        }
        {}
        database = match database_try {
            Some(d) => d,
            None => Database::new(),
        };

        ui.set_database(&database);

        // Fresh databases require a master password before proceeding.
        if fresh_database {
            database.set_path(database_path)?;
            if database.password().is_none() {
                database.set_password(&password);
            }
            if let Err(e) = database.save() {
                ui.set_statusline(&format!("{}", e));
            } else {
                ui.set_statusline(&format!(
                    "New database created at: {}",
                    database_path.display()
                ));
            }
        }

        Ok(Controller { rx, ui, database })
    }

    /// Continuously prompt for a password until either one is provided or the user decides to
    /// quit.
    fn password_prompt(ui: &mut tupm::ui::Ui) -> Option<String> {
        let mut password = None;
        while password.is_none() {
            password = match ui.password_dialog(
                "Please provide a master password for the database:",
                true,
            ) {
                Some(p) => Some(p),
                None => {
                    if ui.yesno_dialog(
                        "Password required",
                        "A master password for the database is required to continue.",
                        "OK",
                        "Exit",
                    )
                    {
                        ui.quit();
                        return None;
                    } else {
                        None
                    }
                }
            };
        }
        password
    }

    /// Run the controller.  This method contains the main loop which will step the UI and process
    /// events until the user quits the application.
    pub fn run(&mut self) {
        while self.ui.step() {
            while let Some(message) = self.next_message() {
                // Dispatch to handler functions as needed.
                match message {
                    Message::AccountEdit(before, after) => self.handle_account_edit(before, after),
                    Message::DatabaseEdit(url, credentials) => {
                        self.handle_database_edit(url, credentials)
                    }
                    Message::Sync => {
                        self.handle_sync(None).ok();
                        ()
                    }
                    Message::ChangePassword(password) => {
                        self.handle_change_password(password);
                    }
                    Message::Quit => {
                        self.ui.quit();
                    }
                };
            }
        }
    }

    /// Return the next message in the message queue, if one is present.
    fn next_message(&self) -> Option<Message> {
        self.rx.try_iter().next()
    }

    /// Process an account change, creation, or deletion.
    fn handle_account_edit(&mut self, before: Option<Account>, after: Option<Account>) {
        let mut modified = false;

        if let (&Some(ref before), &Some(ref after)) = (&before, &after) {
            // Update account
            if before != after {
                if let Err(e) = self.database.update_account(&before.name, &after) {
                    self.ui.set_statusline(&format!("Error: {}", e));
                    return;
                }
                modified = true;
            }
        } else if let (&None, &Some(ref account)) = (&before, &after) {
            // Create account
            if let Err(e) = self.database.add_account(account) {
                self.ui.set_statusline(&format!("Error: {}", e));
                return;
            }
            modified = true;
        } else if let (&Some(ref account), &None) = (&before, &after) {
            // Delete account
            self.database.delete_account(account.name.as_str());
            modified = true;
        }

        if modified {
            self.handle_save_database();
            self.database.clear_synced();
        }

        // Reload the UI with the modified database.
        self.database.accounts.sort();
        self.ui.set_database(&self.database);

        // set_database() will try to preserve the selection based on its index,
        // but since the user can change the account name which can result in the
        // account having a different sorted position, we'll try to re-focus the
        // specific account here.
        if let Some(account) = after {
            self.ui.focus_account(&account.name);
        }

        self.ui.update_status();
    }

    /// Process a change to the database properties (URL, credentials).
    fn handle_database_edit(&mut self, url: String, credentials: String) {
        if (&url, &credentials) != (&self.database.sync_url, &self.database.sync_credentials) {
            self.database.sync_url = url;
            self.database.sync_credentials = credentials;
            self.handle_save_database();
            self.database.clear_synced();
            self.ui.set_database(&self.database);
        }
        self.ui.update_status();
    }

    /// Process a sync.
    fn handle_sync(&mut self, remote_password: Option<&str>) -> Result<(), UpmError> {
        match sync::sync(&self.database, remote_password) {
            Ok(SyncResult::RemoteSynced) => {
                self.ui.set_statusline(&format!(
                    "Remote database synced to revision {}",
                    self.database.sync_revision
                ));
                self.database.set_synced();
                self.ui.set_database(&self.database); // So the UI gets new sync status
                Ok(())
            }
            Ok(SyncResult::LocalSynced) => {
                // Reload local database
                match Database::load_from_file(
                    self.database.path().unwrap(),
                    self.database.password().unwrap(),
                ) {
                    Ok(mut reloaded_database) => {
                        reloaded_database.accounts.sort();
                        self.database = reloaded_database;
                        self.ui.set_database(&self.database);
                        self.ui.set_statusline(&format!(
                            "Local database synced to revision {}",
                            self.database.sync_revision
                        ));
                    }
                    Err(e) => {
                        self.ui.set_statusline(
                            &format!("error reloading local database: {}", e),
                        );
                    }
                };
                self.database.set_synced();
                self.ui.set_database(&self.database); // So the UI gets new sync status
                Ok(())
            }
            Ok(SyncResult::NeitherSynced) => {
                self.ui.set_statusline(&format!(
                    "Both local and remote databases are in sync to revision {}.",
                    self.database.sync_revision
                ));
                self.database.set_synced();
                self.ui.set_database(&self.database); // So the UI gets new sync status
                Ok(())
            }
            Err(UpmError::BadPassword) => {
                if remote_password.is_none() {
                    // Prompt for remote database password and try again
                    let password = self.ui.password_dialog(
                        "The remote database uses a different password.  \
                        Please supply the password to the remote database:",
                        true,
                    );
                    if let Some(password) = password {
                        self.handle_sync(Some(&password))
                    } else {
                        Ok(())
                    }
                } else {
                    // Prevent arbitrary-depth recursion by only asking for the remote database
                    // password once.
                    self.ui.notice_dialog(
                        "Bad password",
                        "Bad password for the remote database.",
                    );
                    self.ui.set_statusline(&format!(
                        "Cannot sync: Bad password for the remote database."
                    ));
                    Err(UpmError::Sync(
                        String::from("Bad password for the remote database."),
                    ))
                }
            }
            Err(e) => {
                self.ui.set_statusline(&format!("Cannot sync: {}", e));
                Err(UpmError::Sync(format!("Cannot sync: {}", e)))
            }
        }
    }

    /// Process a request to change the database password.
    fn handle_change_password(&mut self, new_password: String) {
        self.database.set_password(&new_password);
        if let Err(e) = self.save_database() {
            self.ui.set_statusline(&format!("{}", e));
        } else {
            self.ui.set_statusline("Password updated.");
        }
        self.database.clear_synced();
        self.ui.set_database(&self.database);
    }

    /// Save the database to the local filesystem.  This is the basic function which increments the
    /// revision and makes any needed backups before saving.
    fn save_database(&mut self) -> Result<(), UpmError> {
        // Bump the revision
        self.database.sync_revision += 1;

        // Make a backup of the old database, if present.
        if upm::PARANOID_BACKUPS {
            if let Some(f) = self.database.path() {
                if let Err(e) = backup(&f) {
                    return Err(UpmError::Backup(
                        format!("Error making backup; not saved: {}", e),
                    ));
                }
            }
        }

        // Save the database
        self.database.save()?;
        Ok(())
    }

    /// Save the database to the local filesystem.  This is the function called when the user
    /// explicitly requests a save.  It calls save_database(), processes the result, and updates
    /// the UI status line accordingly.
    fn handle_save_database(&mut self) {
        match self.save_database() {
            Ok(()) => {
                self.ui.set_statusline(&format!(
                    "Database saved to {}",
                    self.database.path().unwrap().display()
                ));
            }
            Err(e) => {
                self.ui.set_statusline(&format!("{}", e));
            }
        };
        self.ui.update_status();
    }
}
