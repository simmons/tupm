//! User interface components for the Terminal Universal Password Manager.

extern crate clap;
extern crate upm;

use cursive;
use cursive::align::HAlign;
use cursive::event::Event::{Char, CtrlChar};
use cursive::event::Key;
use cursive::menu::MenuItem;
use cursive::menu::MenuTree;
use cursive::view::*;
use cursive::views::*;
use cursive::Cursive;
use std::cell::Cell;
use std::cell::RefCell;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::mpsc;
use tupm::clipboard::clipboard_copy;
use tupm::controller;
use upm::database::{Account, Database};

// View ids.  These are used to reference specific views within the Cursive view tree.
static VIEW_ID_SELECT: &'static str = "select";
static VIEW_ID_DETAIL: &'static str = "detail";
static VIEW_ID_FILTER: &'static str = "filter";
static VIEW_ID_REVISION: &'static str = "revision";
static VIEW_ID_MODIFIED: &'static str = "modified";
static VIEW_ID_COUNT: &'static str = "count";
static VIEW_ID_STATUSLINE: &'static str = "statusline";
static VIEW_ID_EDIT: &'static str = "edit";
static VIEW_ID_MODAL: &'static str = "modal";
static VIEW_ID_INPUT: &'static str = "input";

// Human-readable field labels
const FIELD_NAME: &'static str = "Account";
const FIELD_USER: &'static str = "Username";
const FIELD_PASSWORD: &'static str = "Password";
const FIELD_URL: &'static str = "URL";
const FIELD_NOTES: &'static str = "Notes";

/// Describe a specific account field.
struct Field {
    name: &'static str,
    secret: bool,
    multiline: bool,
}

/// Provide a description of each account field.
static FIELDS: [Field; 5] = [
    Field {
        name: FIELD_NAME,
        secret: false,
        multiline: false,
    },
    Field {
        name: FIELD_USER,
        secret: false,
        multiline: false,
    },
    Field {
        name: FIELD_PASSWORD,
        secret: true,
        multiline: false,
    },
    Field {
        name: FIELD_URL,
        secret: false,
        multiline: false,
    },
    Field {
        name: FIELD_NOTES,
        secret: false,
        multiline: true,
    },
];

////////////////////////////////////////////////////////////////////////
// KeyOverrideView
////////////////////////////////////////////////////////////////////////

use cursive::event::{Callback, Event, EventResult};
use cursive::view::{View, ViewWrapper};
use std::collections::HashMap;
use std::collections::HashSet;

/// This view works similarly to the KeyEventView, but the logic has been reversed -- instead of
/// handling only events in our callback list that the child has ignored, we always handle events
/// in the callback list without offering them to the child at all.  Also, an ignored event list is
/// available to simply shield the child from receiving a particular event.
pub struct KeyOverrideView<T: View> {
    content: T,
    config: KeyConfig,
}

impl<T: View> KeyOverrideView<T> {
    /// Create a new KeyOverrideView which wraps the provided view.
    pub fn new(view: T) -> Self {
        KeyOverrideView {
            content: view,
            config: KeyConfig {
                callbacks: Rc::new(RefCell::new(HashMap::new())),
                ignored: Rc::new(RefCell::new(HashSet::new())),
            },
        }
    }

    /// Add an event which should be ignored instead of passed to the interior view.
    pub fn ignore<E: Into<Event>>(mut self, event: E) -> Self {
        // Proxy to KeyConfig
        self.config = self.config.ignore(event);
        self
    }

    /// Register a closure to handle an event, instead of passing the event to the interior view.
    pub fn register<F, E: Into<Event>>(mut self, event: E, cb: F) -> Self
    where
        F: Fn(&mut Cursive) + 'static,
    {
        // Proxy to KeyConfig
        self.config = self.config.register(event, cb);
        self
    }

    /// Register a callback to handle an event, instead of passing the event to the interior view.
    pub fn register_callback<E: Into<Event>>(mut self, event: E, cb: Callback) -> Self {
        // Proxy to KeyConfig
        self.config = self.config.register_callback(event, cb);
        self
    }

    /// Return a KeyConfig struct for this view, which allows changing the event handling
    /// configuration after the view has been integrated into the Cursive view tree.
    pub fn get_config(&self) -> KeyConfig {
        KeyConfig {
            callbacks: self.config.callbacks.clone(),
            ignored: self.config.ignored.clone(),
        }
    }
}

impl<T: View> ViewWrapper for KeyOverrideView<T> {
    wrap_impl!(self.content: T);

    /// Wrap the on_event method to intercept events before they are delivered to the interior
    /// view.
    fn wrap_on_event(&mut self, event: Event) -> EventResult {
        if self.config.ignored.borrow().contains(&event) {
            EventResult::Ignored
        } else {
            match self.config.callbacks.borrow().get(&event) {
                None => self.content.on_event(event.clone()),
                Some(cb) => EventResult::Consumed(Some(cb.clone())),
            }
        }
    }
}

/// KeyConfig allows callers a means of configuring keyboard shortcuts even after the view wrapper
/// has been installed in the Cursive view tree.
#[derive(Clone)]
pub struct KeyConfig {
    callbacks: Rc<RefCell<HashMap<Event, Callback>>>,
    ignored: Rc<RefCell<HashSet<Event>>>,
}

impl KeyConfig {
    /// Add an event which should be ignored instead of passed to the interior view.
    #[allow(dead_code)]
    pub fn ignore<E: Into<Event>>(self, event: E) -> Self {
        self.ignored.borrow_mut().insert(event.into());
        self
    }

    /// Register a closure to handle an event, instead of passing the event to the interior view.
    #[allow(dead_code)]
    pub fn register<F, E: Into<Event>>(self, event: E, cb: F) -> Self
    where
        F: Fn(&mut Cursive) + 'static,
    {
        self.callbacks
            .borrow_mut()
            .insert(event.into(), Callback::from_fn(cb));
        self
    }

    /// Register a callback to handle an event, instead of passing the event to the interior view.
    pub fn register_callback<E: Into<Event>>(self, event: E, cb: Callback) -> Self {
        self.callbacks.borrow_mut().insert(event.into(), cb);
        self
    }
}

////////////////////////////////////////////////////////////////////////
// AccountSelectView
////////////////////////////////////////////////////////////////////////

/// Provide a view for selecting accounts in the database.  This view wraps a Cursive SelectView,
/// and supports filtering the list.
pub struct AccountSelectView {
    content: SelectView<Account>,
    database: Rc<RefCell<Database>>,
    filter: String,
    displayed_accounts: Vec<String>,
}

impl AccountSelectView {
    /// Create a new AccountSelectView representing the accounts in the provided database.
    pub fn new(database: Rc<RefCell<Database>>) -> Self {
        AccountSelectView {
            content: SelectView::<Account>::new(),
            database,
            filter: String::new(),
            displayed_accounts: vec![],
        }
    }

    /// Load accounts from a new database.
    pub fn load(&mut self, database: Rc<RefCell<Database>>) {
        self.database = database;
        self.render();
    }

    /// Render the view by populating the interior SelectView with the relevant accounts.
    fn render(&mut self) {
        self.clear();
        self.displayed_accounts.clear();
        let database = self.database.borrow();
        for account in database.accounts.iter() {
            if self.filter.is_empty() || account.name.contains(&self.filter) {
                self.content.add_item(account.name.clone(), account.clone());

                // Maintain a list of displayed account names since
                // Cursive's SelectView doesn't expose these details
                // of the data model.
                self.displayed_accounts.push(account.name.clone());
            }
        }
    }

    /// Configure a submit callback.  This proxies to the SelectView method.
    pub fn set_on_submit<F>(&mut self, cb: F)
    where
        F: Fn(&mut Cursive, &Account) + 'static,
    {
        self.content.set_on_submit(cb)
    }

    /// Configure a select callback.  This proxies to the SelectView method.
    pub fn set_on_select<F>(&mut self, cb: F)
    where
        F: Fn(&mut Cursive, &Account) + 'static,
    {
        self.content.set_on_select(cb)
    }

    /// Return the currently selected account, if any.
    pub fn selection(&self) -> Option<Rc<Account>> {
        if self.content.is_empty() {
            None
        } else {
            Some(self.content.selection())
        }
    }

    /// Clear the list.
    pub fn clear(&mut self) {
        self.content.clear();
    }

    /// Filter the account list based on the provided substring filter.
    pub fn filter(&mut self, text: &str) {
        self.filter = String::from(text);
        self.render();
    }

    /// Return the total number of accounts.
    pub fn count(&self) -> usize {
        self.database.borrow().accounts.len()
    }

    /// Return the number of accounts which are being shown.
    /// (I.e., accounts that match whatever filter may be in effect.)
    pub fn display_count(&self) -> usize {
        self.content.len()
    }
}

impl ViewWrapper for AccountSelectView {
    wrap_impl!(self.content: SelectView<Account>);
}

////////////////////////////////////////////////////////////////////////
// AccountEditView
////////////////////////////////////////////////////////////////////////

/// This view provides an account edit dialog to create a new account or edit an existing account.
pub struct AccountEditView {
    content: LinearLayout,
    account: Account,
}

impl AccountEditView {
    /// Create a new AccountEditView.
    pub fn new(account: Account) -> Self {
        let mut v_layout = LinearLayout::vertical();

        let field_max = FIELDS.into_iter().map(|f| f.name.len()).max().unwrap();
        let labelify = |name: &str| {
            let mut s = String::with_capacity(field_max);
            s.push_str(name);
            s.push_str(": ");
            for _ in 0..(field_max - name.len()) {
                s.push(' ');
            }
            s
        };

        for field in FIELDS.into_iter() {
            let id = format!("{}_{}", VIEW_ID_EDIT, field.name);
            let mut edit_view = EditView::new();
            edit_view.set_secret(field.secret);
            if !field.multiline {
                v_layout.add_child(
                    LinearLayout::horizontal()
                        .child(TextView::new(labelify(field.name)))
                        .child(BoxView::new(
                            SizeConstraint::AtLeast(30),
                            SizeConstraint::AtMost(1),
                            edit_view.with_id(id),
                        )),
                );
            } else {
                v_layout.add_child(
                    LinearLayout::vertical()
                        .child(TextView::new(labelify(field.name)))
                        .child(BoxView::new(
                            SizeConstraint::AtLeast(30),
                            SizeConstraint::Fixed(10),
                            TextArea::new().with_id(id),
                        )),
                );
            }
        }
        v_layout.add_child(TextView::new("Ctrl-R: Reveal password"));
        v_layout.add_child(TextView::new("Ctrl-X: Apply changes"));

        let mut account_edit = AccountEditView {
            content: v_layout,
            account: account,
        };
        account_edit.load();
        account_edit
    }

    /// Provision a new dialog box containing an AccountEditView and some basic handlers.
    pub fn show(
        cursive: &mut Cursive,
        database: Rc<RefCell<Database>>,
        controller_tx: mpsc::Sender<controller::Message>,
        account: Option<&Account>,
    ) {
        let create: bool;
        let account = match account {
            Some(account) => {
                create = false;
                account.clone()
            }
            None => {
                create = true;
                Account::new()
            }
        };

        let account_edit = AccountEditView::new(account.clone()).with_id(VIEW_ID_EDIT);
        let controller_tx_clone = controller_tx.clone();
        let database_clone = database.clone();
        let key_override = KeyOverrideView::new(account_edit)
            .register(cursive::event::Event::CtrlChar('r'), |s| {
                // reveal password
                if let Some(mut account_edit) = s.find_id::<AccountEditView>(VIEW_ID_EDIT) {
                    account_edit.reveal_password();
                }
            })
            .register(cursive::event::Event::CtrlChar('x'), move |s| {
                AccountEditView::apply(s, database_clone.clone(), &controller_tx_clone)
            });
        let controller_tx_clone = controller_tx.clone();
        let database_clone = database.clone();
        cursive.add_layer(
            Dialog::around(key_override)
                .title(if create {
                    "New account..."
                } else {
                    "Edit account..."
                })
                .button("Apply", move |s| {
                    AccountEditView::apply(s, database_clone.clone(), &controller_tx_clone)
                })
                .dismiss_button("Cancel"),
        );
    }

    /// Handle the CTRL-R "reveal password" feature.
    fn reveal_password(&mut self) {
        let id = format!("{}_{}", VIEW_ID_EDIT, FIELD_PASSWORD);
        self.find_id(&id, |edit_view: &mut EditView| {
            edit_view.set_secret(false);
        });
    }

    /// Populate a UI field with a value.
    fn put(&mut self, field_name: &str, value: &str) {
        let id = format!("{}_{}", VIEW_ID_EDIT, field_name);
        if FIELDS
            .into_iter()
            .any(|f| f.name == field_name && f.multiline)
        {
            self.find_id(&id, |edit_view: &mut TextArea| edit_view.set_content(value));
        } else {
            self.find_id(&id, |edit_view: &mut EditView| edit_view.set_content(value));
        }
    }

    /// Retrieve the text from a UI field.
    fn get(&mut self, field_name: &str) -> String {
        let id = format!("{}_{}", VIEW_ID_EDIT, field_name);

        if FIELDS
            .into_iter()
            .any(|f| f.name == field_name && f.multiline)
        {
            match self.find_id(&id, |edit_view: &mut TextArea| {
                String::from(edit_view.get_content())
            }) {
                Some(x) => x,
                None => String::from(""),
            }
        } else {
            match self.find_id(&id, |edit_view: &mut EditView| edit_view.get_content()) {
                Some(x) => (*x).clone(),
                None => String::from(""),
            }
        }
    }

    /// Load the fields from the contained account object into the UI.
    fn load(&mut self) {
        let account = self.account.clone();
        self.put(FIELD_NAME, &account.name);
        self.put(FIELD_USER, &account.user);
        self.put(FIELD_PASSWORD, &account.password);
        self.put(FIELD_URL, &account.url);
        self.put(FIELD_NOTES, &account.notes);
    }

    /// Return an account object representing the current state of the UI fields.
    fn current(&mut self) -> Account {
        Account {
            name: self.get(FIELD_NAME),
            user: self.get(FIELD_USER),
            password: self.get(FIELD_PASSWORD),
            url: self.get(FIELD_URL),
            notes: self.get(FIELD_NOTES),
        }
    }

    /// Update the database with the information contained in the form.
    fn apply(
        cursive: &mut Cursive,
        database: Rc<RefCell<Database>>,
        controller_tx: &mpsc::Sender<controller::Message>,
    ) {
        // We can't have references to both the AccountEditView and
        // AccountSelectView in the same lexical scope, since it causes
        // a BorrowMutError for some reason.  So, extract the needed
        // information from AccountEditView before updating the database
        // contained within AccountSelectView.
        let (name, previous, current) =
            if let Some(mut account_edit) = cursive.find_id::<AccountEditView>(VIEW_ID_EDIT) {
                (
                    account_edit.account.name.clone(),
                    account_edit.account.clone(),
                    account_edit.current(),
                )
            } else {
                return;
            };

        // Check for name collision
        if name != current.name && database.borrow().contains(&current.name) {
            cursive.add_layer(
                Dialog::around(TextView::new(
                    "Another account already exists with this name.",
                ))
                .title("Alert")
                .button("OK", |s| s.pop_layer()),
            );
            return;
        }
        cursive.screen_mut().pop_layer();

        // Send the account edit request to the controller
        let before = if previous.name.is_empty() {
            None
        } else {
            Some(previous)
        };
        controller_tx
            .send(controller::Message::AccountEdit(before, Some(current)))
            .unwrap();
    }
}

impl ViewWrapper for AccountEditView {
    wrap_impl!(self.content: LinearLayout);
}

////////////////////////////////////////////////////////////////////////
// DatabaseEditView
////////////////////////////////////////////////////////////////////////

/// Edit the database properties.
pub struct DatabaseEditView {
    content: LinearLayout,
    url: String,
    credentials: String,
}

impl DatabaseEditView {
    /// Create a new DatabaseEditView.
    pub fn new(url: &str, credentials: &str) -> Self {
        let mut v_layout = LinearLayout::vertical();

        let id = format!("{}_{}", VIEW_ID_EDIT, "url");
        let mut edit_view = EditView::new();
        edit_view.set_content(url);
        v_layout.add_child(
            LinearLayout::horizontal()
                .child(TextView::new("Sync URL:         "))
                .child(BoxView::new(
                    SizeConstraint::AtLeast(50),
                    SizeConstraint::AtMost(1),
                    edit_view.with_id(id),
                )),
        );

        let id = format!("{}_{}", VIEW_ID_EDIT, "credentials");
        let mut edit_view = EditView::new();
        edit_view.set_content(credentials);
        v_layout.add_child(
            LinearLayout::horizontal()
                .child(TextView::new("Sync credentials: "))
                .child(BoxView::new(
                    SizeConstraint::AtLeast(50),
                    SizeConstraint::AtMost(1),
                    edit_view.with_id(id),
                )),
        );

        v_layout.add_child(TextView::new("Ctrl-X: Apply changes"));
        v_layout.add_child(TextView::new(
            "The sync credentials must exactly match the name of an \
             account which holds the HTTP Basic Authentication username \
             and password.",
        ));

        DatabaseEditView {
            content: v_layout,
            url: String::from(url),
            credentials: String::from(credentials),
        }
    }

    /// Provision a new dialog box containing a DatabaseEditView and some basic handlers.
    pub fn show(
        cursive: &mut Cursive,
        database: Rc<RefCell<Database>>,
        controller_tx: mpsc::Sender<controller::Message>,
    ) {
        let database_edit = DatabaseEditView::new(
            &database.borrow().sync_url,
            &database.borrow().sync_credentials,
        )
        .with_id(VIEW_ID_EDIT);
        let controller_tx_clone = controller_tx.clone();
        let key_override = KeyOverrideView::new(database_edit).register(
            cursive::event::Event::CtrlChar('x'),
            move |s| {
                DatabaseEditView::apply(s, &controller_tx_clone);
            },
        );
        let controller_tx_clone = controller_tx.clone();
        cursive.add_layer(
            Dialog::around(key_override)
                .title("Database Properties...")
                .button("Apply", move |s| {
                    DatabaseEditView::apply(s, &controller_tx_clone)
                })
                .dismiss_button("Cancel"),
        );
    }

    /// Record the (potentially edited) UI fields into the database.
    fn apply(cursive: &mut Cursive, controller_tx: &mpsc::Sender<controller::Message>) {
        let (old_url, old_credentials) = {
            let database_edit = cursive.find_id::<DatabaseEditView>(VIEW_ID_EDIT).unwrap();
            (database_edit.url.clone(), database_edit.credentials.clone())
        };

        let id = format!("{}_{}", VIEW_ID_EDIT, "url");
        let new_url = cursive.find_id::<EditView>(&id).unwrap().get_content();

        let id = format!("{}_{}", VIEW_ID_EDIT, "credentials");
        let new_credentials = cursive.find_id::<EditView>(&id).unwrap().get_content();

        if (&old_url, &old_credentials) != (&new_url, &new_credentials) {
            controller_tx
                .send(controller::Message::DatabaseEdit(
                    (*new_url).clone(),
                    (*new_credentials).clone(),
                ))
                .unwrap();
        }
        cursive.screen_mut().pop_layer();
    }
}

impl ViewWrapper for DatabaseEditView {
    wrap_impl!(self.content: LinearLayout);
}

////////////////////////////////////////////////////////////////////////
// Ui
////////////////////////////////////////////////////////////////////////

/// The UI maintains a message queue consisting of zero or more of these messages.  Other
/// components can add messages to the queue, and the UI will process them in order.
#[derive(Debug)]
pub enum UiMessage {
    UpdateStatus,
    ShowAccountEdit(Option<Account>),
    ShowDatabaseEdit,
    RequireSync,
    ChangePassword,
    Refresh,
}

/// Provide the user interface.  This struct owns the Cursive instance and all data needed to
/// handle user interaction.
pub struct Ui {
    cursive: Cursive,
    ui_rx: mpsc::Receiver<UiMessage>,
    ui_tx: mpsc::Sender<UiMessage>,
    controller_tx: mpsc::Sender<controller::Message>,
    database: Rc<RefCell<Database>>,
}

impl Ui {
    /// Create a new Ui object.  The provided `mpsc` sender will be used by the UI to send messages
    /// to the controller.
    pub fn new(controller_tx: mpsc::Sender<controller::Message>) -> Ui {
        let (ui_tx, ui_rx) = mpsc::channel::<UiMessage>();
        let mut ui = Ui {
            cursive: Cursive::new(),
            ui_tx,
            ui_rx,
            controller_tx,
            database: Rc::new(RefCell::new(Database::new())),
        };

        ////////////////////////////////////////////////////////////
        // Construct the Cursive view hierarchy for our user interface.
        ////////////////////////////////////////////////////////////

        let mut account_list = AccountSelectView::new(ui.database.clone());

        let mut account_detail = TextView::new("");
        account_detail.set_scrollable(true);
        let account_detail = account_detail.with_id(VIEW_ID_DETAIL);

        let account_detail_panel = Panel::new(BoxView::new(
            // Hack to make the detail panel consume the rest of the horizontal space.  Full wasn't
            // working when the SelectView had a large number of accounts and scrollbar was
            // present.
            SizeConstraint::AtLeast(500),
            SizeConstraint::Free,
            account_detail,
        ));

        let ui_tx_clone = ui.ui_tx.clone();
        account_list.set_on_select(move |s, account| {
            let mut detail = match s.find_id::<TextView>(VIEW_ID_DETAIL) {
                Some(x) => x,
                None => return,
            };
            detail.set_content(render_account_text(account, false));
            ui_tx_clone.send(UiMessage::UpdateStatus).unwrap();
        });

        let ui_tx_clone = ui.ui_tx.clone();
        let database_clone = ui.database.clone();
        account_list.set_on_submit(move |_, account| {
            let account = account.clone();
            let ui_tx_clone2 = ui_tx_clone.clone();
            if sync_guard(&database_clone.borrow(), &ui_tx_clone2) {
                return;
            } else {
                ui_tx_clone2
                    .send(UiMessage::ShowAccountEdit(Some(account.clone())))
                    .unwrap();
            }
        });

        let account_list_key_override =
            KeyOverrideView::new(account_list.with_id(VIEW_ID_SELECT)).ignore('/');
        let account_list_keys = account_list_key_override.get_config();

        let account_list_panel = Panel::new(BoxView::new(
            SizeConstraint::AtLeast(20),
            SizeConstraint::Free,
            account_list_key_override,
        ));

        let mut h_layout = LinearLayout::horizontal();
        h_layout.add_child(account_list_panel);
        h_layout.add_child(account_detail_panel);

        let body = BoxView::new(SizeConstraint::Full, SizeConstraint::Full, h_layout);

        let ui_tx_clone = ui.ui_tx.clone();
        let filter_edit = EditView::new().on_edit(move |s, text, _| {
            let details = match s.find_id::<AccountSelectView>(VIEW_ID_SELECT) {
                Some(mut account_list) => {
                    account_list.filter(text);
                    account_list
                        .selection()
                        .map(|a| render_account_text(&a, false))
                }
                None => None,
            };
            match s.find_id::<TextView>(VIEW_ID_DETAIL) {
                Some(mut account_detail) => {
                    match details {
                        Some(details) => account_detail.set_content(details),
                        None => account_detail.set_content(""),
                    };
                }
                None => {}
            };
            ui_tx_clone.send(UiMessage::UpdateStatus).unwrap();
        });
        let filter_edit = filter_edit.with_id(VIEW_ID_FILTER);

        let revision_text = TextView::new("").with_id(VIEW_ID_REVISION);
        let modified_text = TextView::new("").with_id(VIEW_ID_MODIFIED);
        let count_text = TextView::new("").with_id(VIEW_ID_COUNT);
        let statusline_text = TextView::new("").with_id(VIEW_ID_STATUSLINE);

        let help_text = TextView::new("Press escape or \\ for menu.");
        let status_layout = LinearLayout::horizontal()
            .child(TextView::new("filter: "))
            .child(BoxView::new(
                SizeConstraint::AtLeast(14),
                SizeConstraint::Free,
                filter_edit,
            ))
            .weight(10)
            .child(TextView::new(" | "))
            .child(revision_text)
            .child(modified_text)
            .child(TextView::new(" | "))
            .child(count_text);
        let status_layout = LinearLayout::vertical()
            .child(status_layout)
            .child(help_text)
            .child(statusline_text);
        let status_box = BoxView::new(
            SizeConstraint::Full,
            SizeConstraint::Fixed(4),
            status_layout,
        );

        let title = TextView::new("Terminal universal password manager").h_align(HAlign::Center);
        let layout = LinearLayout::vertical()
            .child(title)
            .child(body)
            .weight(100)
            .child(status_box);
        let main_dialog = BoxView::new(SizeConstraint::Full, SizeConstraint::Full, layout);

        ////////////////////////////////////////////////////////////
        // Callbacks
        ////////////////////////////////////////////////////////////

        // Even though these are lightweight clones, it is still a shame that we need to go through
        // this awkward dance to use these items within closures.
        let controller_tx_clone1 = ui.controller_tx.clone();
        let controller_tx_clone2 = ui.controller_tx.clone();
        let controller_tx_clone3 = ui.controller_tx.clone();
        let ui_tx_clone1 = ui.ui_tx.clone();
        let ui_tx_clone2 = ui.ui_tx.clone();
        let ui_tx_clone3 = ui.ui_tx.clone();
        let ui_tx_clone4 = ui.ui_tx.clone();
        let ui_tx_clone5 = ui.ui_tx.clone();
        let database_clone1 = ui.database.clone();
        let database_clone2 = ui.database.clone();

        let do_focus_filter = Callback::from_fn(|s| {
            let _ = s.focus_id(VIEW_ID_FILTER);
        });

        let do_clipboard_copy_username = Callback::from_fn(|s| {
            match selected_account(s) {
                Some(account) => {
                    match clipboard_copy(account.user.as_str()) {
                        Ok(_) => (),
                        Err(e) => {
                            let dialog = Dialog::info(e).title("Error while copying to clipboard:");
                            s.add_layer(dialog);
                        }
                    };
                }
                None => {}
            };
        });

        let do_clipboard_copy_password = Callback::from_fn(|s| {
            match selected_account(s) {
                Some(account) => {
                    match clipboard_copy(account.password.as_str()) {
                        Ok(_) => (),
                        Err(e) => {
                            let dialog = Dialog::info(e).title("Error while copying to clipboard:");
                            s.add_layer(dialog);
                        }
                    };
                }
                None => {}
            };
        });

        let do_reveal_password = Callback::from_fn(|s| {
            let account = match selected_account(s) {
                Some(account) => account,
                None => return,
            };
            match s.find_id::<TextView>(VIEW_ID_DETAIL) {
                Some(mut detail) => detail.set_content(render_account_text(&account, true)),
                None => {}
            };
        });

        let do_new_account = Callback::from_fn(move |_| {
            if sync_guard(&database_clone1.borrow(), &ui_tx_clone1) {
                return;
            } else {
                ui_tx_clone1.send(UiMessage::ShowAccountEdit(None)).unwrap();
            }
        });

        let do_delete_account = Callback::from_fn(move |s| {
            if let Some(account) = selected_account(s) {
                if sync_guard(&database_clone2.borrow(), &ui_tx_clone2) {
                    return;
                }
                let controller_tx_clone = controller_tx_clone1.clone();
                s.add_layer(
                    Dialog::around(TextView::new(format!(
                        "Really delete account \"{}\"?",
                        account.name
                    )))
                    .title("Confirm")
                    .button("No", |s| s.pop_layer())
                    .button("Yes", move |s| {
                        controller_tx_clone
                            .send(controller::Message::AccountEdit(
                                Some((*account).clone()),
                                None,
                            ))
                            .unwrap();
                        s.pop_layer();
                    }),
                );
            }
        });

        let do_sync = Callback::from_fn(move |_| {
            controller_tx_clone2
                .send(controller::Message::Sync)
                .unwrap();
        });

        let do_edit_database = Callback::from_fn(move |_| {
            ui_tx_clone3.send(UiMessage::ShowDatabaseEdit).unwrap();
        });

        let do_change_password = Callback::from_fn(move |_| {
            ui_tx_clone4.send(UiMessage::ChangePassword).unwrap();
        });

        let do_quit = Callback::from_fn(move |_| {
            controller_tx_clone3
                .send(controller::Message::Quit)
                .unwrap();
        });

        let do_refresh = Callback::from_fn(move |_| {
            ui_tx_clone5.send(UiMessage::Refresh).unwrap();
        });

        ////////////////////////////////////////////////////////////
        // Menu bar
        ////////////////////////////////////////////////////////////

        // We don't use the more idiomatic builder syntax for
        // constructing menus, but instead manually build the data
        // structure of each MenuTree.  This allows us to provide
        // Callback structs instead of closures.  We use Callback
        // structs instead of closures so we can define our callbacks
        // above, and reuse them for several bindings (e.g. menu items
        // and key shortcuts).

        let mut file_menu = MenuTree::new();
        file_menu.children = vec![MenuItem::Leaf(String::from("Quit ^X"), do_quit.clone())];
        let mut database_menu = MenuTree::new();
        database_menu.children = vec![
            MenuItem::Leaf(String::from("Sync Database            ^Y"), do_sync.clone()),
            MenuItem::Leaf(
                String::from("Edit Database Properties ^K"),
                do_edit_database.clone(),
            ),
            MenuItem::Leaf(String::from("Change Database Password"), do_change_password),
        ];
        let mut account_menu = MenuTree::new();
        account_menu.children = vec![
            MenuItem::Leaf(String::from("New Account     ^N"), do_new_account.clone()),
            MenuItem::Leaf(
                String::from("Delete Account  ^D"),
                do_delete_account.clone(),
            ),
            MenuItem::Leaf(
                String::from("Copy Username   ^U"),
                do_clipboard_copy_username.clone(),
            ),
            MenuItem::Leaf(
                String::from("Copy Password   ^P"),
                do_clipboard_copy_password.clone(),
            ),
            MenuItem::Leaf(
                String::from("Reveal Password ^R"),
                do_reveal_password.clone(),
            ),
        ];
        ui.cursive
            .menubar()
            .add_subtree("File", file_menu)
            .add_subtree("Database", database_menu)
            .add_subtree("Account", account_menu);
        ui.cursive.set_autohide_menu(false);

        ////////////////////////////////////////////////////////////
        // Key shortcuts
        ////////////////////////////////////////////////////////////

        let main_key_override = KeyOverrideView::new(main_dialog)
            // / : Focus the filter edit view
            .register_callback(Char('/'), do_focus_filter)
            // Ctrl-U: Copy username to clipboard
            .register_callback(CtrlChar('u'), do_clipboard_copy_username)
            // Ctrl-P: Copy password to clipboard
            .register_callback(CtrlChar('p'), do_clipboard_copy_password)
            // Ctrl-R: Reveal password
            .register_callback(CtrlChar('r'), do_reveal_password)
            // Ctrl-N: New account
            .register_callback(CtrlChar('n'), do_new_account)
            // Ctrl-D/Backspace/Delete: Delete account
            .register_callback(CtrlChar('d'), do_delete_account.clone())
            // Ctrl-Y: Sync
            .register_callback(CtrlChar('y'), do_sync)
            // Ctrl-K: Database Information
            .register_callback(CtrlChar('k'), do_edit_database)
            // Ctrl-X: Quit
            .register_callback(CtrlChar('x'), do_quit)
            // Backslash: Menu bar
            .register(Char('\\'), |s| s.select_menubar());

        account_list_keys
            .register_callback(Key::Backspace, do_delete_account.clone())
            .register_callback(Key::Del, do_delete_account);

        ui.cursive.add_layer(main_key_override);

        ////////////////////////////////////////////////////////////
        // Global key shortcuts
        ////////////////////////////////////////////////////////////

        // Escape key: Pop layers, unless the main layer is active, in which case quit.
        ui.cursive.add_global_callback(Key::Esc, |s| {
            if s.screen().layer_sizes().len() > 1 {
                s.pop_layer();
            } else {
                s.select_menubar();
            }
        });

        // Ctrl-L: Refresh screen
        ui.cursive
            .add_global_callback(CtrlChar('l'), move |s| do_refresh(s));

        ui
    }

    /// Load a new database (or an updated version of the existing database) into the UI.
    pub fn set_database(&mut self, database: &Database) {
        *self.database.borrow_mut() = database.clone();
        match self.cursive.find_id::<AccountSelectView>(VIEW_ID_SELECT) {
            Some(mut account_list) => {
                let previous_selection = account_list.content.selected_id();
                account_list.load(self.database.clone());
                // If possible, restore the previous account
                // selection after a new database is loaded.
                match previous_selection {
                    Some(previous_selection) => {
                        if previous_selection < account_list.content.len() {
                            account_list.content.set_selection(previous_selection);
                        }
                    }
                    None => {}
                };
            }
            _ => {}
        }
        self.update_detail();
        self.update_status();
    }

    /// Change the current selection to focus on the account as
    /// specified by its name.  If no account with that name is present,
    /// then the selection is not changed.
    pub fn focus_account(&mut self, account_name: &str) {
        if let Some(mut account_list) = self.cursive.find_id::<AccountSelectView>(VIEW_ID_SELECT) {
            let mut target_index: Option<usize> = None;

            for (index, name) in account_list.displayed_accounts.iter().enumerate() {
                if name == account_name {
                    target_index = Some(index);
                    break;
                }
            }
            if let Some(index) = target_index {
                account_list.content.set_selection(index);
            }
        };
        self.update_detail();
    }

    /// Retrieve the next available UiMessage to process.
    pub fn next_ui_message(&self) -> Option<UiMessage> {
        self.ui_rx.try_iter().next()
    }

    /// Step the UI by calling into Cursive's step function, then processing any UI messages.
    pub fn step(&mut self) -> bool {
        if !self.cursive.is_running() {
            return false;
        }

        // Step the UI
        self.cursive.step();

        // Process any UI messages
        while let Some(message) = self.next_ui_message() {
            match message {
                UiMessage::UpdateStatus => self.update_status(),
                UiMessage::ShowAccountEdit(a) => self.handle_show_account_edit(a),
                UiMessage::ShowDatabaseEdit => self.handle_show_database_edit(),
                UiMessage::RequireSync => self.handle_require_sync(),
                UiMessage::ChangePassword => self.handle_change_password(),
                UiMessage::Refresh => self.handle_refresh(),
            }
        }
        true
    }

    /// Handle UiMessage::ShowAccountEdit messages.
    fn handle_show_account_edit(&mut self, account: Option<Account>) {
        match account {
            Some(a) => AccountEditView::show(
                &mut self.cursive,
                self.database.clone(),
                self.controller_tx.clone(),
                Some(&a),
            ),
            None => AccountEditView::show(
                &mut self.cursive,
                self.database.clone(),
                self.controller_tx.clone(),
                None,
            ),
        };
    }

    /// Handle UiMessage::ShowDatabaseEdit messages.
    fn handle_show_database_edit(&mut self) {
        DatabaseEditView::show(
            &mut self.cursive,
            self.database.clone(),
            self.controller_tx.clone(),
        );
    }

    /// Handle UiMessage::RequireSync messages.
    fn handle_require_sync(&mut self) {
        let text = "The database should be synchronized before editing \
                    accounts.  Synchronize now?";
        let controller_tx_clone = self.controller_tx.clone();
        self.cursive.add_layer(
            Dialog::around(TextView::new(text))
                .button("No", |s| {
                    s.pop_layer();
                })
                .button("Yes", move |s| {
                    s.pop_layer();
                    controller_tx_clone.send(controller::Message::Sync).unwrap();
                    // It would be nice to open the account edit
                    // dialog here, but the account data will be
                    // potentially stale until the controller
                    // processes the Sync.
                })
                .title("Database not synchronized"),
        );
    }

    /// Handle UiMessage::ChangePassword messages.
    fn handle_change_password(&mut self) {
        let password = self.password_dialog(
            "Please provide a new master password for this new database:",
            false,
        );
        let password = match password {
            Some(p) => p,
            None => return,
        };

        self.controller_tx
            .send(controller::Message::ChangePassword(password))
            .unwrap();
    }

    /// Handle UiMessage::Refresh messages.
    fn handle_refresh(&mut self) {
        self.cursive.clear();
    }

    /// Quit.
    pub fn quit(&mut self) {
        self.cursive.quit();
    }

    /// Present a modal dialog to the user and step the UI until the dialog is dismissed.  This is
    /// a synchronous operation, and will not return until the dialog is finished.
    fn modal_dialog(&mut self, dialog: Dialog) {
        self.cursive.add_layer(dialog.with_id(VIEW_ID_MODAL));
        while self.cursive.is_running() && self.cursive.find_id::<Dialog>(VIEW_ID_MODAL).is_some() {
            self.cursive.step();
        }
    }

    /// Present a modal confirmation dialog to the user and step the UI until the dialog is
    /// dismissed.  Returns true if the button with "true_text" was selected; otherwise false.
    ///
    /// This is a synchronous operation, and will not return until the dialog is finished.
    pub fn yesno_dialog(
        &mut self,
        title: &str,
        text: &str,
        false_text: &str,
        true_text: &str,
    ) -> bool {
        let result = Rc::new(Cell::new(false));
        {
            let result = result.clone();
            self.modal_dialog(
                Dialog::around(TextView::new(text))
                    .button(false_text, |s| {
                        s.pop_layer();
                    })
                    .button(true_text, move |s| {
                        result.set(true);
                        s.pop_layer();
                    })
                    .title(title),
            );
        }
        result.get()
    }

    /// Present a modal dialog to the user displaying a short notice.
    ///
    /// Step the UI until the dialog is dismissed.  This is a synchronous operation, and will not
    /// return until the dialog is finished.
    pub fn notice_dialog(&mut self, title: &str, text: &str) {
        self.modal_dialog(
            Dialog::around(TextView::new(text))
                .button("OK", move |s| {
                    s.pop_layer();
                })
                .title(title),
        );
    }

    /// Present a modal password dialog to the user and step the UI until the dialog is dismissed.
    /// Returns a password if one was provided, otherwise returns None if the password field was
    /// left empty or cancel was selected.  This is a synchronous operation, and will not return
    /// until the dialog is finished.
    pub fn password_dialog(&mut self, text: &str, secret: bool) -> Option<String> {
        let result = Rc::new(RefCell::new(None));
        {
            let result_clone1 = result.clone();
            let result_clone2 = result.clone();
            let mut editview = EditView::new().on_submit(move |s, text| {
                if !text.is_empty() {
                    *result_clone1.borrow_mut() = Some(String::from(text));
                }
                s.pop_layer();
                s.focus_id(VIEW_ID_SELECT).ok();
            });
            editview.set_secret(secret);
            let layout = LinearLayout::vertical()
                .child(TextView::new(text))
                .child(editview.with_id(VIEW_ID_INPUT));
            self.modal_dialog(
                Dialog::around(layout)
                    .button("Ok", move |s| {
                        let text = s.find_id::<EditView>(VIEW_ID_INPUT).unwrap().get_content();
                        if !text.is_empty() {
                            *result_clone2.borrow_mut() = Some((*text).clone());
                        }
                        s.pop_layer();
                        s.focus_id(VIEW_ID_SELECT).ok();
                    })
                    .dismiss_button("Cancel")
                    .title("Enter password"),
            );
        }
        let result = match *result.borrow() {
            Some(ref s) => Some(s.clone()),
            None => None,
        };
        result
    }

    /// The internals of the AccountSelectView can't push details of the selected account directly
    /// to the detail TextView, since it doesn't have a reference to the toplevel Cursive.
    /// Therefore, we need this independent function.
    fn update_detail(&mut self) {
        let details = match self.cursive.find_id::<AccountSelectView>(VIEW_ID_SELECT) {
            Some(account_list) => {
                match account_list
                    .selection()
                    .map(|a| render_account_text(&a, false))
                {
                    Some(details) => details,
                    None => String::from(""),
                }
            }
            None => String::from(""),
        };
        let mut account_detail = match self.cursive.find_id::<TextView>(VIEW_ID_DETAIL) {
            Some(account_detail) => account_detail,
            None => return,
        };
        account_detail.set_content(details);
    }

    /// Update the UI status information: count, revision, etc.
    pub fn update_status(&mut self) {
        let (counts, revision) = match self.cursive.find_id::<AccountSelectView>(VIEW_ID_SELECT) {
            Some(account_list) => (
                (account_list.display_count(), account_list.count()),
                self.database.borrow().sync_revision,
            ),
            None => ((0, 0), 0),
        };
        if let Some(mut count_text) = self.cursive.find_id::<TextView>(VIEW_ID_COUNT) {
            count_text.set_content(format!("{}/{} accounts", counts.0, counts.1));
        };
        if let Some(mut revision_text) = self.cursive.find_id::<TextView>(VIEW_ID_REVISION) {
            if revision != 0 {
                revision_text.set_content(format!("Revision {}", revision));
            } else {
                revision_text.set_content("")
            };
        };
        if let Some(mut modified_text) = self.cursive.find_id::<TextView>(VIEW_ID_MODIFIED) {
            if !self.database.borrow().is_synced() {
                modified_text.set_content(" UNSYNCHRONIZED");
            } else {
                modified_text.set_content("");
            }
        };
    }

    /// Update the status line.
    pub fn set_statusline(&mut self, text: &str) {
        match self.cursive.find_id::<TextView>(VIEW_ID_STATUSLINE) {
            Some(mut statusline_text) => {
                statusline_text.set_content(text);
            }
            None => {}
        }
    }
}

/// Return a reference to the currently selected account.
fn selected_account(mut cursive: &mut Cursive) -> Option<Rc<Account>> {
    let select = cursive
        .find_id::<AccountSelectView>(VIEW_ID_SELECT)
        .unwrap();
    select.selection()
}

/// Render account details into a single text string.
fn render_account_text(account: &Account, reveal_password: bool) -> String {
    fn indent_multiline(value: &str) -> String {
        // TODO: Ideally, this would be smart about preserving indentation for long strings that
        // wrap around.
        String::from(value.trim().replace("\n", "\n          ").as_str())
    }
    fn render_line(text: &mut String, field: &str, value: &String) {
        let mut label = String::from(field);
        label.push(':');
        text.push_str(&(format!("{:10}{}\n", label, indent_multiline(value)))[..]);
    };
    let password;
    if reveal_password {
        password = account.password.clone();
    } else {
        password = String::from("************");
    };
    let mut text = String::new();
    render_line(&mut text, FIELD_NAME, &account.name);
    render_line(&mut text, FIELD_USER, &account.user);
    render_line(&mut text, FIELD_PASSWORD, &password);
    render_line(&mut text, FIELD_URL, &account.url);
    render_line(&mut text, FIELD_NOTES, &account.notes);
    text
}

/// Confirm that the database has been recently synced.  If it hasn't, then return true and arrange
/// for a "sync?" dialog box to be presented.
fn sync_guard<T>(database: &T, channel: &mpsc::Sender<UiMessage>) -> bool
where
    T: Deref<Target = Database>,
{
    if database.has_remote() && !database.is_synced() {
        channel.send(UiMessage::RequireSync).unwrap();
        true
    } else {
        false
    }
}
