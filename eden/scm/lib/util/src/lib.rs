/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! Utilities interacting with the OS.

// What functions belong here? The theme is similar to mercurial/util.py
//
// Cross platform filesystem / network / process / string / data structures
// utilities that cannot be trivially written using Rust stdlib.
//
// Prefer using the Rust stdlib directly if possible.

mod bgprocess;
pub mod file;
pub mod lock;
pub mod path;

pub use bgprocess::run_background;
