//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Binary generation records retain the common Storage codecs inside the native materialization and its encryption domain.

use super::{NativeColumnType::Blob, NativeRecordFamily, NativeRecordLayout};

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
