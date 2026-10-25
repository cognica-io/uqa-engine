//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One integration harness for Rust and native C ABI verification.

#[path = "cases/array_values.rs"]
mod array_values;

#[path = "cases/config.rs"]
mod config;

#[path = "cases/copy.rs"]
mod copy;

#[path = "cases/cursors.rs"]
mod cursors;

#[path = "cases/dense_values.rs"]
mod dense_values;

#[path = "cases/native.rs"]
mod native;

#[path = "cases/notifications.rs"]
mod notifications;

#[path = "cases/sessions.rs"]
mod sessions;

#[path = "cases/statements.rs"]
mod statements;

#[path = "cases/values.rs"]
mod values;

#[path = "cases/record_values.rs"]
mod record_values;

#[path = "cases/range_values.rs"]
mod range_values;
