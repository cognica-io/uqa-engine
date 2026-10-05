//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Server metadata has its own family so adding SQL identities does not re-encode retained server history.

use super::{NativeColumnType::Text, NativeRecordFamily as Family, NativeRecordLayout};

pub(crate) const SQL: &str = "CREATE TABLE _foreign_server_metadata (name TEXT PRIMARY KEY NOT NULL, metadata TEXT NOT NULL) WITHOUT ROWID";

pub(super) const LAYOUT: NativeRecordLayout = NativeRecordLayout {
    family: Family::ForeignServerMetadata,
    table: "_foreign_server_metadata",
    columns: &["name", "metadata"],
    column_types: &[Text, Text],
    nullable: &[false; 2],
    primary_key: &[0],
    identity_columns: &[0],
    object_owned: false,
};
