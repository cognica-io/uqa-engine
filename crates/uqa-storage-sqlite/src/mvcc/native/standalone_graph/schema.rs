//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped standalone graph rows retain their separate property encoding and never share catalog graph identities.

use super::super::{NativeColumnType, NativeRecordFamily as Family, NativeRecordLayout};
use NativeColumnType::{Integer, Text};

pub(in crate::mvcc::native) const TABLES: &[(Family, &str)] = &[
    (Family::StandaloneGraphScopes, "CREATE TABLE _uqa_mvcc_native_standalone_graph_scopes (scope TEXT NOT NULL, legacy_suffix TEXT, PRIMARY KEY(scope)) WITHOUT ROWID"),
    (Family::StandaloneGraphMetadata, "CREATE TABLE _uqa_mvcc_native_standalone_graph_metadata (scope TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, PRIMARY KEY(scope, key), FOREIGN KEY(scope) REFERENCES _uqa_mvcc_native_standalone_graph_scopes(scope)) WITHOUT ROWID"),
    (Family::StandaloneGraphCatalog, "CREATE TABLE _uqa_mvcc_native_standalone_graph_catalog (scope TEXT NOT NULL, name TEXT NOT NULL, registry_json TEXT NOT NULL, PRIMARY KEY(scope, name), FOREIGN KEY(scope) REFERENCES _uqa_mvcc_native_standalone_graph_scopes(scope)) WITHOUT ROWID"),
    (Family::StandaloneGraphVertices, "CREATE TABLE _uqa_mvcc_native_standalone_graph_vertices (scope TEXT NOT NULL, vertex_id INTEGER NOT NULL, label TEXT NOT NULL, properties_json TEXT NOT NULL, properties_format INTEGER NOT NULL, PRIMARY KEY(scope, vertex_id), CHECK(properties_format IN (1, 2)), FOREIGN KEY(scope) REFERENCES _uqa_mvcc_native_standalone_graph_scopes(scope)) WITHOUT ROWID"),
    (Family::StandaloneGraphEdges, "CREATE TABLE _uqa_mvcc_native_standalone_graph_edges (scope TEXT NOT NULL, edge_id INTEGER NOT NULL, source_id INTEGER NOT NULL, target_id INTEGER NOT NULL, label TEXT NOT NULL, properties_json TEXT NOT NULL, properties_format INTEGER NOT NULL, PRIMARY KEY(scope, edge_id), CHECK(properties_format IN (1, 2)), FOREIGN KEY(scope) REFERENCES _uqa_mvcc_native_standalone_graph_scopes(scope)) WITHOUT ROWID"),
    (Family::StandaloneGraphMembership, "CREATE TABLE _uqa_mvcc_native_standalone_graph_membership (scope TEXT NOT NULL, entity_type TEXT NOT NULL, entity_id INTEGER NOT NULL, graph_name TEXT NOT NULL, PRIMARY KEY(scope, entity_type, entity_id, graph_name), CHECK(entity_type IN ('vertex', 'edge')), FOREIGN KEY(scope) REFERENCES _uqa_mvcc_native_standalone_graph_scopes(scope)) WITHOUT ROWID"),
    (Family::StandaloneGraphLookups, "CREATE TABLE _uqa_mvcc_native_standalone_graph_lookups (scope TEXT NOT NULL, kind TEXT NOT NULL, text_key TEXT NOT NULL, integer_key INTEGER NOT NULL, entity_type TEXT NOT NULL, entity_id INTEGER NOT NULL, PRIMARY KEY(scope, kind, text_key, integer_key, entity_type, entity_id), FOREIGN KEY(scope) REFERENCES _uqa_mvcc_native_standalone_graph_scopes(scope)) WITHOUT ROWID"),
];

pub(in crate::mvcc::native) const LAYOUTS: &[NativeRecordLayout] = &[
    NativeRecordLayout {
        family: Family::StandaloneGraphScopes,
        table: "_uqa_mvcc_native_standalone_graph_scopes",
        columns: &["scope", "legacy_suffix"],
        column_types: &[Text, Text],
        nullable: &[false, true],
        primary_key: &[0],
        identity_columns: &[0],
        object_owned: false,
    },
    NativeRecordLayout {
        family: Family::StandaloneGraphMetadata,
        table: "_uqa_mvcc_native_standalone_graph_metadata",
        columns: &["scope", "key", "value"],
        column_types: &[Text, Text, Text],
        nullable: &[false, false, false],
        primary_key: &[0, 1],
        identity_columns: &[0, 1],
        object_owned: false,
    },
    NativeRecordLayout {
        family: Family::StandaloneGraphCatalog,
        table: "_uqa_mvcc_native_standalone_graph_catalog",
        columns: &["scope", "name", "registry_json"],
        column_types: &[Text, Text, Text],
        nullable: &[false, false, false],
        primary_key: &[0, 1],
        identity_columns: &[0, 1],
        object_owned: false,
    },
    NativeRecordLayout {
        family: Family::StandaloneGraphVertices,
        table: "_uqa_mvcc_native_standalone_graph_vertices",
        columns: &[
            "scope",
            "vertex_id",
            "label",
            "properties_json",
            "properties_format",
        ],
        column_types: &[Text, Integer, Text, Text, Integer],
        nullable: &[false, false, false, false, false],
        primary_key: &[0, 1],
        identity_columns: &[0, 1],
        object_owned: false,
    },
    NativeRecordLayout {
        family: Family::StandaloneGraphEdges,
        table: "_uqa_mvcc_native_standalone_graph_edges",
        columns: &[
            "scope",
            "edge_id",
            "source_id",
            "target_id",
            "label",
            "properties_json",
            "properties_format",
        ],
        column_types: &[Text, Integer, Integer, Integer, Text, Text, Integer],
        nullable: &[false, false, false, false, false, false, false],
        primary_key: &[0, 1],
        identity_columns: &[0, 1],
        object_owned: false,
    },
    NativeRecordLayout {
        family: Family::StandaloneGraphMembership,
        table: "_uqa_mvcc_native_standalone_graph_membership",
        columns: &["scope", "entity_type", "entity_id", "graph_name"],
        column_types: &[Text, Text, Integer, Text],
        nullable: &[false, false, false, false],
        primary_key: &[0, 1, 2, 3],
        identity_columns: &[0, 1, 2, 3],
        object_owned: false,
    },
    NativeRecordLayout {
        family: Family::StandaloneGraphLookups,
        table: "_uqa_mvcc_native_standalone_graph_lookups",
        columns: &[
            "scope",
            "kind",
            "text_key",
            "integer_key",
            "entity_type",
            "entity_id",
        ],
        column_types: &[Text, Text, Text, Integer, Text, Integer],
        nullable: &[false, false, false, false, false, false],
        primary_key: &[0, 1, 2, 3, 4, 5],
        identity_columns: &[0, 1, 2, 3, 4, 5],
        object_owned: false,
    },
];
