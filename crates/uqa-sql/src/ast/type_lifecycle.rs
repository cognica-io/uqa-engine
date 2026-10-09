//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `ALTER TYPE | DOMAIN ... RENAME TO | SET SCHEMA | OWNER TO`.

use serde::{Deserialize, Serialize};

use super::RoleSpecification;

/// The keyword that named the target. `DOMAIN` statements reject types that are not domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeObjectKind {
    Type,
    Domain,
}

/// One `ALTER TYPE` or `ALTER DOMAIN` lifecycle change of a user-defined type and its generated array type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlterTypeObject {
    pub kind: TypeObjectKind,
    /// Rendered, possibly qualified type name as written.
    pub name: String,
    pub action: AlterTypeObjectAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlterTypeObjectAction {
    /// The new unqualified name.
    RenameTo(String),
    /// The destination schema.
    SetSchema(String),
    OwnerTo(RoleSpecification),
    /// Rename an attribute without changing its type or attribute number.
    RenameAttribute {
        from: String,
        to: String,
    },
    /// Append attributes to a standalone composite relation in written order.
    AddAttributes(Vec<super::CompositeAttributeAddition>),
    /// `PostgreSQL` prepares types against the original descriptor, then executes DROP, TYPE and ADD groups in order.
    AlterAttributes {
        removals: Vec<super::CompositeAttributeRemoval>,
        #[serde(default)]
        type_changes: Vec<super::CompositeAttributeDefinition>,
        additions: Vec<super::CompositeAttributeAddition>,
    },
}
