//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The access methods of `pg_am`: `PostgreSQL` 18's own table and index methods, and the vector index methods the engine adds.

/// The type of an access method (`pg_am.amtype`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessMethodKind {
    Table,
    Index,
}

/// Every access method by name, in `pg_am` order.
const ACCESS_METHODS: &[(&str, AccessMethodKind)] = &[
    ("heap", AccessMethodKind::Table),
    ("btree", AccessMethodKind::Index),
    ("hash", AccessMethodKind::Index),
    ("gist", AccessMethodKind::Index),
    ("gin", AccessMethodKind::Index),
    ("brin", AccessMethodKind::Index),
    ("spgist", AccessMethodKind::Index),
    ("ivf", AccessMethodKind::Index),
    ("hnsw", AccessMethodKind::Index),
    ("diskann", AccessMethodKind::Index),
];

/// The type of the access method `name`, which names one exactly, or `None` when no access method has that name.
pub fn access_method_kind(name: &str) -> Option<AccessMethodKind> {
    ACCESS_METHODS
        .iter()
        .find(|(method, _)| *method == name)
        .map(|(_, kind)| *kind)
}
