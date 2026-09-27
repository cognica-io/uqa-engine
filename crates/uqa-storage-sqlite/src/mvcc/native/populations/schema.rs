//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact native populations are field-owned materializations of shared Storage metadata.

use super::super::{
    NativeColumnType::{Blob, Integer, Text},
    NativeRecordFamily as Family, NativeRecordLayout,
};

pub(in crate::mvcc::native) const TABLES: [(Family, &str); 2] = [
    (Family::VectorPopulations, "CREATE TABLE _uqa_mvcc_native_vector_populations (table_name TEXT NOT NULL, field TEXT NOT NULL, generation BLOB NOT NULL CHECK(typeof(generation) = 'blob' AND length(generation) = 40), population BLOB NOT NULL CHECK(typeof(population) = 'blob' AND length(population) = 72), PRIMARY KEY (table_name, field, generation)) WITHOUT ROWID"),
    (Family::VectorPopulationWitnesses, "CREATE TABLE _uqa_mvcc_native_vector_population_witnesses (table_name TEXT NOT NULL, field TEXT NOT NULL, generation BLOB NOT NULL CHECK(typeof(generation) = 'blob' AND length(generation) = 40), doc_id INTEGER NOT NULL CHECK(doc_id >= 0), witness BLOB NOT NULL CHECK(typeof(witness) = 'blob' AND length(witness) = 120), PRIMARY KEY (table_name, field, generation, doc_id)) WITHOUT ROWID"),
];

pub(in crate::mvcc::native) const HEADER: NativeRecordLayout = NativeRecordLayout {
    family: Family::VectorPopulations,
    table: "_uqa_mvcc_native_vector_populations",
    columns: &["table_name", "field", "generation", "population"],
    column_types: &[Text, Text, Blob, Blob],
    nullable: &[false; 4],
    primary_key: &[0, 1, 2],
    identity_columns: &[1, 2],
    object_owned: true,
};

pub(in crate::mvcc::native) const WITNESS: NativeRecordLayout = NativeRecordLayout {
    family: Family::VectorPopulationWitnesses,
    table: "_uqa_mvcc_native_vector_population_witnesses",
    columns: &["table_name", "field", "generation", "doc_id", "witness"],
    column_types: &[Text, Text, Blob, Integer, Blob],
    nullable: &[false; 5],
    primary_key: &[0, 1, 2, 3],
    identity_columns: &[1, 2, 3],
    object_owned: true,
};
