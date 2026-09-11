//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema DROP target authority, namespace protection, and empty-schema validation.

use crate::{
    catalog::{is_virtual_system_schema, security::SchemaSecurity},
    SQLError,
};
use std::collections::BTreeSet;

pub trait EmptySchemaCatalog {
    fn schema_registered(&self, name: &str) -> bool;
    fn schema_is_empty(&self, name: &str) -> bool;
}
pub trait SchemaDropCatalog: EmptySchemaCatalog {
    fn schema_security(&self, name: &str) -> Option<SchemaSecurity>;
    fn current_user_has_role_privileges(&self, role: &str) -> bool;
    fn schema_is_graph(&self, name: &str) -> Result<bool, String>;
}
pub enum BoundSchemaDrop {
    Schema,
    Graph,
    Skipped(String),
}

pub fn bind_schema_drop_target(
    catalog: &dyn SchemaDropCatalog,
    name: &str,
    if_exists: bool,
) -> Result<BoundSchemaDrop, SQLError> {
    let Some(security) = catalog.schema_security(name) else {
        if if_exists {
            return Ok(BoundSchemaDrop::Skipped(format!(
                "schema \"{name}\" does not exist, skipping"
            )));
        }
        return Err(SQLError::Routine {
            sqlstate: "3F000".into(),
            message: format!("schema \"{name}\" does not exist"),
        });
    };
    if !catalog.current_user_has_role_privileges(&security.role_owner) {
        return Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of schema {name}"),
        });
    }
    if is_virtual_system_schema(name) {
        return Err(SQLError::Routine {
            sqlstate: "2BP01".into(),
            message: format!("schema `{name}` cannot be dropped"),
        });
    }
    if catalog
        .schema_is_graph(name)
        .map_err(|error| SQLError::Internal(format!("DROP SCHEMA: {error}")))?
    {
        Ok(BoundSchemaDrop::Graph)
    } else {
        Ok(BoundSchemaDrop::Schema)
    }
}

pub fn validate_schema_drop_restrict(
    catalog: &dyn SchemaDropCatalog,
    schemas: &BTreeSet<String>,
    graphs: &BTreeSet<String>,
) -> Result<(), SQLError> {
    let occupied = schemas
        .iter()
        .find(|name| !catalog.schema_is_empty(name))
        .or_else(|| graphs.first());
    if let Some(name) = occupied {
        let single = schemas.len() + graphs.len() == 1;
        let object = if single {
            format!("schema {name}")
        } else {
            "desired object(s)".into()
        };
        return Err(SQLError::Routine {
            sqlstate: "2BP01".into(),
            message: format!(
                "cannot drop {object} because other objects depend on {}",
                if single { "it" } else { "them" }
            ),
        });
    }
    Ok(())
}

pub fn validate_empty_schema_drop(
    catalog: &dyn EmptySchemaCatalog,
    name: &str,
) -> Result<bool, String> {
    if is_virtual_system_schema(name) {
        return Err(format!("schema `{name}` cannot be dropped"));
    }
    if !catalog.schema_registered(name) {
        return Ok(false);
    }
    if !catalog.schema_is_empty(name) {
        return Err(format!("schema `{name}` is not empty"));
    }
    Ok(true)
}

pub fn routine_name_occupies_schema(name: &str, schema: &str) -> bool {
    uqa_core::RelationIdentity::from_legacy_name(name)
        .map_or(true, |relation| relation.schema == schema)
}
