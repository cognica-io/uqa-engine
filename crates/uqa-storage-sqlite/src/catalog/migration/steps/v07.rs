//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable analyzer, foreign-data, index, and graph-path registries.

pub(super) const SQL: &str = r"
    CREATE TABLE IF NOT EXISTS _analyzers (
        name        TEXT PRIMARY KEY,
        config_json TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _table_field_analyzers (
        table_name    TEXT NOT NULL,
        field         TEXT NOT NULL,
        phase         TEXT NOT NULL,
        analyzer_name TEXT NOT NULL,
        PRIMARY KEY (table_name, field, phase)
    );

    CREATE TABLE IF NOT EXISTS _foreign_servers (
        name     TEXT PRIMARY KEY,
        fdw_type TEXT NOT NULL,
        options  TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _foreign_tables (
        name         TEXT PRIMARY KEY,
        server_name  TEXT NOT NULL,
        columns_json TEXT NOT NULL,
        options      TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _catalog_indexes (
        name       TEXT PRIMARY KEY,
        index_type TEXT NOT NULL,
        table_name TEXT NOT NULL,
        columns    TEXT NOT NULL,
        parameters TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _path_indexes (
        graph_name      TEXT PRIMARY KEY,
        label_sequences TEXT NOT NULL
    );
    ";
