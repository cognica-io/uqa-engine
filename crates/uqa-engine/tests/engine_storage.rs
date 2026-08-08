//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consolidated backend, persistence, session, and concurrency tests.

#[path = "catalog_atomicity.rs"]
mod catalog_atomicity;
#[path = "concurrency.rs"]
mod concurrency;
#[path = "direct_rmw_concurrency.rs"]
mod direct_rmw_concurrency;
#[path = "engine_sessions.rs"]
mod engine_sessions;
#[path = "python_db_migration.rs"]
mod python_db_migration;
#[path = "redb_backend.rs"]
mod redb_backend;
#[path = "sql_callback_transactions.rs"]
mod sql_callback_transactions;
#[path = "sql_cancellation.rs"]
mod sql_cancellation;
#[path = "sqlite_backend_parity.rs"]
mod sqlite_backend_parity;
#[path = "sqlite_compression.rs"]
mod sqlite_compression;
#[path = "sqlite_deep_model.rs"]
mod sqlite_deep_model;
#[path = "sqlite_dml_reopen.rs"]
mod sqlite_dml_reopen;
#[path = "sqlite_encryption.rs"]
mod sqlite_encryption;
#[path = "sqlite_key_value_backend.rs"]
mod sqlite_key_value_backend;
#[path = "transaction_lifecycle.rs"]
mod transaction_lifecycle;
