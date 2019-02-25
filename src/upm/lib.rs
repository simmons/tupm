//! This crate provides functions for reading, writing, and synchronizing Universal Password
//! Manager (UPM) version 3 databases.  This code is meant to interoperate with the format used by
//! [the original UPM Java application](https://github.com/adrian/upm-swing).
//!
//! A terminal-based interface to UPM databases (tupm) is provided as an example application.

extern crate rand;
extern crate reqwest;
extern crate time;

#[cfg(test)]
#[macro_use]
extern crate matches;

pub mod backup;
mod crypto;
pub mod database;
pub mod error;
mod openssl_extra;
pub mod sync;

/// If this is true, we'll back backups to both the local filesystem and
/// the remote sync server.  This is a safeguard against our code
/// clobbering the database.
pub const PARANOID_BACKUPS: bool = true;

/// Log formatted messages to stderr, but only for debug builds.
#[macro_export]
#[cfg(debug_assertions)]
macro_rules! log(
    ($($arg:tt)*) => { {
        use std::io::prelude::*;
        let r = writeln!(&mut ::std::io::stderr(), $($arg)*);
        r.expect("failed printing to stderr");
    } }
);

#[macro_export]
#[cfg(not(debug_assertions))]
macro_rules! log(
    ($($arg:tt)*) => { {
    } }
);
