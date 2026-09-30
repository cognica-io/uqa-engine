//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! User-defined enum declarations and their bound type references.

use serde::{Deserialize, Serialize};
use uqa_core::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

/// `CREATE TYPE name AS ENUM (...)` with labels in declaration order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateEnum {
    /// Rendered qualified type name, resolved against the creation namespace during execution.
    pub name: String,
    pub labels: Vec<String>,
}

/// `ALTER TYPE name ADD VALUE ...` or `ALTER TYPE name RENAME VALUE ...`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlterEnum {
    pub name: String,
    pub action: AlterEnumAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlterEnumAction {
    AddValue {
        label: String,
        if_not_exists: bool,
        neighbor: Option<EnumNeighbor>,
    },
    RenameValue {
        old: String,
        new: String,
    },
}

/// The existing label that a new label is placed before or after.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumNeighbor {
    pub label: String,
    pub after: bool,
}

/// A user-defined enum type bound by catalog identity. The schema and name are the catalog's current names; equality compares the type identity only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumTypeReference {
    pub schema: String,
    pub name: String,
    pub oid: u32,
    pub array_oid: u32,
}

impl PartialEq for EnumTypeReference {
    fn eq(&self, other: &Self) -> bool {
        self.oid == other.oid
    }
}

impl Eq for EnumTypeReference {}

impl EnumTypeReference {
    /// Copy the owned catalog names under the production reservation; the identities are inline.
    pub fn clone_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        let schema = control.copy_text(&self.schema)?;
        let name = control.copy_text(&self.name)?;
        let (schema, schema_memory) = schema.into_parts();
        let (name, name_memory) = name.into_parts();
        control.finish(
            Self {
                schema,
                name,
                oid: self.oid,
                array_oid: self.array_oid,
            },
            control.combine(schema_memory, name_memory),
        )
    }
}
