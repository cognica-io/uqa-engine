//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

/// A part of an upgrade of the native mapping that writes rows of the
/// database again.
///
/// The other parts of an upgrade create tables, triggers and guards. They
/// take no time that grows with the database and are not reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StorageUpgradeStep {
    /// The selectors of graphs and of their paths. Written for a database
    /// that is stored in a mapping before 3.
    GraphLookups,
    /// The values of sequences. Written for a database that is stored in a
    /// mapping before 14.
    SequenceValues,
    /// The lookup key of every B-tree index entry, and the index over those
    /// keys. Written for a database that is stored in a mapping before 18.
    LookupKeys,
}

impl StorageUpgradeStep {
    /// A name for a log: lowercase words joined by underscores.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::GraphLookups => "graph_lookups",
            Self::SequenceValues => "sequence_values",
            Self::LookupKeys => "lookup_keys",
        }
    }
}
