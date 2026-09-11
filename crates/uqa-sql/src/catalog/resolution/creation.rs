//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Creation namespaces and index-target lookup over the caller's actual metadata guards.

use super::candidates::{relation_lookup_candidates, RelationCandidateState};
use crate::catalog::{
    roles::RoleReferenceNames,
    security::{
        schema::SchemaAclPrivilege,
        schema_inquiry::{SchemaPrivilegeCatalog, SchemaPrivilegeInquiry},
    },
};
use crate::SQLError;
use uqa_core::RelationIdentity;

pub trait CreationRelationNames {
    fn contains(&self, relation: &RelationIdentity) -> bool;
}
pub trait CreationRelationGuards {
    fn tables(&self) -> Box<dyn CreationRelationNames + '_>;
    fn views(&self) -> Box<dyn CreationRelationNames + '_>;
    fn sequences(&self) -> Box<dyn CreationRelationNames + '_>;
    fn foreign_tables(&self) -> Box<dyn CreationRelationNames + '_>;
    fn indexes(&self) -> Box<dyn CreationRelationNames + '_>;
}

pub fn temporary_creation_parts(
    state: &dyn RelationCandidateState,
    name: &str,
) -> Result<(String, String), SQLError> {
    let (schema, relation) =
        RelationIdentity::parse_reference(name).map_err(SQLError::Unsupported)?;
    let temporary_schema = state.temporary_schema_name();
    if schema
        .as_deref()
        .is_some_and(|schema| schema != "pg_temp" && schema != temporary_schema)
    {
        return Err(SQLError::Unsupported(
            "temporary relations cannot specify a schema name".into(),
        ));
    }
    Ok((temporary_schema, relation))
}

pub fn api_relation_name(
    state: &dyn RelationCandidateState,
    catalog: &dyn SchemaPrivilegeCatalog,
    name: &str,
) -> Result<String, String> {
    let (schema, relation) = RelationIdentity::parse_reference(name)?;
    if let Some(schema) = schema {
        if !catalog.schemas().contains_key(&schema) {
            return Err(format!("schema `{schema}` does not exist"));
        }
        return Ok(RelationIdentity::new(schema, relation).qualified_name());
    }
    let search_path = state.search_path();
    let schemas = catalog.schemas();
    let schema = search_path
        .iter()
        .find(|schema| {
            schema.as_str() != "pg_catalog"
                && schema.as_str() != "information_schema"
                && schemas.contains_key(schema.as_str())
        })
        .cloned()
        .ok_or_else(|| "no schema has been selected to create in".to_string())?;
    Ok(RelationIdentity::new(schema, relation).qualified_name())
}

pub fn sql_creation_schema(
    state: &dyn RelationCandidateState,
    privileges: &SchemaPrivilegeInquiry<'_>,
    schema: Option<&str>,
    current_user: &str,
) -> Option<String> {
    if let Some(schema) = schema {
        privileges
            .schema_security_for_privilege(schema)
            .is_some()
            .then(|| schema.to_string())
    } else {
        let search_path = state.search_path().clone();
        search_path.into_iter().find(|schema| {
            privileges.schema_security_for_privilege(schema).is_some()
                && privileges.schema_has_privilege_for_role(
                    schema,
                    current_user,
                    SchemaAclPrivilege::Usage,
                )
        })
    }
}

pub fn missing_creation_schema(schema: Option<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "3F000".into(),
        message: schema.map_or_else(
            || "no schema has been selected to create in".into(),
            |schema| format!("schema \"{schema}\" does not exist"),
        ),
    }
}

pub fn ensure_creation_privilege(
    names: &dyn RoleReferenceNames,
    privileges: &SchemaPrivilegeInquiry<'_>,
    canonical_name: &str,
) -> Result<(), SQLError> {
    let relation =
        RelationIdentity::from_legacy_name(canonical_name).map_err(SQLError::Unsupported)?;
    let current_user = names.current_user_name();
    privileges.require_schema_privilege(&relation.schema, &current_user, SchemaAclPrivilege::Create)
}

pub fn resolve_index_table_name(
    names: &dyn RoleReferenceNames,
    state: &dyn RelationCandidateState,
    privileges: &SchemaPrivilegeInquiry<'_>,
    catalog: &dyn CreationRelationGuards,
    name: &str,
) -> Result<Option<String>, SQLError> {
    let (qualified_schema, _) =
        RelationIdentity::parse_reference(name).map_err(SQLError::Unsupported)?;
    if let Some(schema) = qualified_schema.as_deref() {
        if schema != "pg_temp" && schema != state.temporary_schema_name() {
            if privileges.schema_security_for_privilege(schema).is_none() {
                return Err(SQLError::Routine {
                    sqlstate: "3F000".into(),
                    message: format!("schema \"{schema}\" does not exist"),
                });
            }
            let current_user = names.current_user_name();
            privileges.require_schema_privilege(
                schema,
                &current_user,
                SchemaAclPrivilege::Usage,
            )?;
        }
    }
    let current_user = names.current_user_name();
    for relation in relation_lookup_candidates(state, name)
        .map_err(|error| SQLError::Internal(format!("resolve index table `{name}`: {error}")))?
    {
        if qualified_schema.is_none()
            && relation.schema != state.temporary_schema_name()
            && !privileges.schema_has_privilege_for_role(
                &relation.schema,
                &current_user,
                SchemaAclPrivilege::Usage,
            )
        {
            continue;
        }
        if catalog.tables().contains(&relation) {
            return Ok(Some(relation.qualified_name()));
        }
        if catalog.views().contains(&relation)
            || catalog.sequences().contains(&relation)
            || catalog.foreign_tables().contains(&relation)
            || catalog.indexes().contains(&relation)
        {
            return Ok(None);
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
