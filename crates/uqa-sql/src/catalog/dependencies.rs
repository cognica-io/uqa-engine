//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog dependencies as `PostgreSQL` records them in `pg_depend` and `pg_shdepend`, and the deletion search and reports that `DROP` makes from them.

mod address;
mod deletion;
mod edge;
mod graph;
mod shared;

pub use address::{
    ObjectAddress, ATTRIBUTE_DEFAULT_CLASS, CONSTRAINT_CLASS, DATABASE_CLASS, LANGUAGE_CLASS,
    NAMESPACE_CLASS, PROCEDURE_CLASS, RELATION_CLASS, REWRITE_CLASS, ROLE_CLASS,
    ROLE_MEMBERSHIP_CLASS, TRIGGER_CLASS, TYPE_CLASS,
};
pub use deletion::{CascadeNotice, DeletionFlags, DeletionTarget, DeletionTargets};
pub use edge::{Dependency, DependencyKind};
pub use graph::DependencyGraph;
pub use shared::{shared_dependency_detail, SharedDependency, SharedDependencyKind};

/// `getObjectDescription`: an object's description, `None` for an object that does not exist.
pub type Describe<'a> = &'a dyn Fn(ObjectAddress) -> Result<Option<String>, crate::SQLError>;
