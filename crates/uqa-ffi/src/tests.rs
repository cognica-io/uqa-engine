//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The single native-library test harness.

#[path = "tests/allocations.rs"]
mod allocations;

#[cfg(feature = "test-support")]
#[path = "tests/panics.rs"]
mod panics;

#[path = "tests/array_values.rs"]
mod array_values;

#[path = "tests/config.rs"]
mod config;

#[path = "tests/copy.rs"]
mod copy;

#[path = "tests/cursors.rs"]
mod cursors;

#[path = "tests/dense_values.rs"]
mod dense_values;

#[path = "tests/headers.rs"]
mod headers;

#[path = "tests/notifications.rs"]
mod notifications;

#[path = "tests/sessions.rs"]
mod sessions;

#[path = "tests/statements.rs"]
mod statements;

#[path = "tests/values.rs"]
mod values;

#[path = "tests/xml_values.rs"]
mod xml_values;

#[path = "tests/record_values.rs"]
mod record_values;

#[path = "tests/range_values.rs"]
mod range_values;

#[cfg(feature = "test-support")]
#[path = "tests/database.rs"]
mod database;

#[cfg(feature = "test-support")]
#[path = "tests/completion.rs"]
mod completion;
