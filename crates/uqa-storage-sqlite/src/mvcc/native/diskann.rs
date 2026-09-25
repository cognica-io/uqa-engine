//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Binary generation records retain the common Storage codecs inside the native materialization and its encryption domain.

use super::{
    NativeColumnType::{Blob, Integer, Text},
    NativeRecordFamily, NativeRecordLayout,
};

pub(super) const SQL: &str = "CREATE TABLE _uqa_mvcc_native_diskann_records (key BLOB PRIMARY KEY NOT NULL CHECK(typeof(key) = 'blob'), value BLOB NOT NULL CHECK(typeof(value) = 'blob')) WITHOUT ROWID";

pub(super) const LAYOUT: NativeRecordLayout = NativeRecordLayout {
    family: NativeRecordFamily::DiskANNRecords,
    table: "_uqa_mvcc_native_diskann_records",
    columns: &["key", "value"],
    column_types: &[Blob, Blob],
    nullable: &[false, false],
    primary_key: &[0],
    identity_columns: &[0],
    object_owned: false,
};

pub(super) const ORIGINS_SQL: &str = "CREATE TABLE _uqa_mvcc_native_vector_origins (table_name TEXT NOT NULL, field TEXT NOT NULL, doc_id INTEGER NOT NULL CHECK(doc_id >= 0), origin BLOB NOT NULL CHECK(typeof(origin) = 'blob' AND length(origin) = 56), PRIMARY KEY (table_name, field, doc_id)) WITHOUT ROWID";

pub(super) const ORIGINS_LAYOUT: NativeRecordLayout = NativeRecordLayout {
    family: NativeRecordFamily::VectorOrigins,
    table: "_uqa_mvcc_native_vector_origins",
    columns: &["table_name", "field", "doc_id", "origin"],
    column_types: &[Text, Text, Integer, Blob],
    nullable: &[false; 4],
    primary_key: &[0, 1, 2],
    identity_columns: &[1, 2],
    object_owned: true,
};
