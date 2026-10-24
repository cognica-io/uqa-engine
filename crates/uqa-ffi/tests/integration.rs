//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One integration harness for Rust and native C ABI verification.

#[path = "cases/config.rs"]
mod config;

#[path = "cases/copy.rs"]
mod copy;

#[path = "cases/cursors.rs"]
mod cursors;

#[path = "cases/native.rs"]
mod native;

#[path = "cases/notifications.rs"]
mod notifications;

#[path = "cases/sessions.rs"]
mod sessions;
