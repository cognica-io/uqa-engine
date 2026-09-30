//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One `pg_depend` row: a dependent object, the object it references, and how deleting the referenced object treats it.

use super::ObjectAddress;

/// `pg_depend.deptype`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DependencyKind {
    /// `n`: dropping the referenced object requires `CASCADE`, which drops the dependent.
    Normal,
    /// `a`: the dependent is dropped with the referenced object even without `CASCADE`.
    Auto,
    /// `i`: the dependent is part of the referenced object's implementation and can only be dropped with it.
    Internal,
    /// `P`: a partition's object depends on its parent's.
    PartitionPrimary,
    /// `S`: a partition's object depends on its partition.
    PartitionSecondary,
    /// `e`: the dependent is a member of an extension.
    Extension,
    /// `x`: the dependent needs an extension and is dropped with it.
    AutoExtension,
}

impl DependencyKind {
    /// The `pg_depend.deptype` letter.
    #[must_use]
    pub const fn code(self) -> char {
        match self {
            Self::Normal => 'n',
            Self::Auto => 'a',
            Self::Internal => 'i',
            Self::PartitionPrimary => 'P',
            Self::PartitionSecondary => 'S',
            Self::Extension => 'e',
            Self::AutoExtension => 'x',
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Dependency {
    pub dependent: ObjectAddress,
    pub referenced: ObjectAddress,
    pub kind: DependencyKind,
}
