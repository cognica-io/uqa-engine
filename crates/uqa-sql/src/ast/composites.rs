//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Composite type declarations and their bound type references.

use serde::{Deserialize, Serialize};
use uqa_core::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

use super::ColumnType;

/// `CREATE TYPE name AS (attribute type [COLLATE collation], ...)` with attributes in declaration order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateCompositeType {
    /// Rendered qualified type name, resolved against the creation namespace during execution.
    pub name: String,
    pub attributes: Vec<CompositeAttributeDefinition>,
}

/// One declared attribute. The type is a declaration until execution binds it through the catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositeAttributeDefinition {
    pub name: String,
    pub ty: ColumnType,
    /// The rendered qualified collation name of an explicit `COLLATE` clause.
    pub collation: Option<String>,
    /// `SETOF type`, which the grammar accepts and `BuildDescForRelation` rejects.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub setof: bool,
}

/// One ADD ATTRIBUTE declaration. Unlike CREATE TYPE, ALTER applies column declaration lowering, including SERIAL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositeAttributeAddition {
    pub attribute: CompositeAttributeDefinition,
    pub declaration: super::ColumnDeclaration,
}

/// One DROP ATTRIBUTE action; missing-name handling and dependency behavior remain execution-time decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositeAttributeRemoval {
    pub name: String,
    pub if_exists: bool,
    pub cascade: bool,
}

/// Creation-time positional binding of a stored ROW constructor. Type references use the same canonical-name lifecycle as stored casts; attribute numbers survive additions and dropped slots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositeRowBinding {
    pub ty: String,
    pub attributes: Vec<i16>,
    /// Types of the admitted arguments; absent only in predecessor definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument_types: Option<Vec<ColumnType>>,
}

/// A composite type bound by catalog identity: a standalone composite type or the row type of a relation. The schema and name are the catalog's current names; equality compares the type identity only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositeTypeReference {
    pub schema: String,
    pub name: String,
    /// The row type's `pg_type` OID.
    pub oid: u32,
    pub array_oid: u32,
    /// The `pg_class` OID of the relation that describes the attributes (`pg_type.typrelid`).
    pub relation_oid: u32,
}

impl PartialEq for CompositeTypeReference {
    fn eq(&self, other: &Self) -> bool {
        self.oid == other.oid
    }
}

impl Eq for CompositeTypeReference {}

impl CompositeTypeReference {
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
                relation_oid: self.relation_oid,
            },
            control.combine(schema_memory, name_memory),
        )
    }
}
