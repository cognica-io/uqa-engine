//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Creation namespaces and index-target lookup over the caller's actual metadata guards.

use super::candidates::{relation_lookup_candidates, RelationCandidateState};
use crate::ast::RelationPersistence;
use crate::catalog::temporary_namespace::{is_temporary_schema_name, temporary_toast_schema_name};
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
    fn named_type_exists(&self, identity: &RelationIdentity) -> bool;
    fn tables(&self) -> Box<dyn CreationRelationNames + '_>;
    fn views(&self) -> Box<dyn CreationRelationNames + '_>;
    fn sequences(&self) -> Box<dyn CreationRelationNames + '_>;
    fn foreign_tables(&self) -> Box<dyn CreationRelationNames + '_>;
    fn indexes(&self) -> Box<dyn CreationRelationNames + '_>;
    /// The relations of standalone composite types, which occupy the relation namespace as well as the type namespace.
    fn composite_types(&self) -> Box<dyn CreationRelationNames + '_>;
}

/// Domains and row types share the type namespace; sequences and indexes do not define row types.
pub fn type_name_in_use(catalog: &dyn CreationRelationGuards, identity: &RelationIdentity) -> bool {
    catalog.named_type_exists(identity)
        || catalog.tables().contains(identity)
        || catalog.views().contains(identity)
        || catalog.foreign_tables().contains(identity)
        || catalog.composite_types().contains(identity)
}

pub fn ensure_type_name_available(
    catalog: &dyn CreationRelationGuards,
    identity: &RelationIdentity,
) -> Result<(), SQLError> {
    if type_name_in_use(catalog, identity) {
        return Err(SQLError::Routine {
            sqlstate: "42710".into(),
            message: format!("type \"{}\" already exists", identity.name),
        });
    }
    Ok(())
}

/// Every relation kind shares the same namespace, independently of query visibility.
pub fn relation_name_in_use(
    catalog: &dyn CreationRelationGuards,
    relation: &RelationIdentity,
) -> bool {
    catalog.tables().contains(relation)
        || catalog.views().contains(relation)
        || catalog.sequences().contains(relation)
        || catalog.foreign_tables().contains(relation)
        || catalog.indexes().contains(relation)
        || catalog.composite_types().contains(relation)
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
    current_user: &(impl crate::catalog::roles::identity::RoleSubject + ?Sized),
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

/// The schema `recomputeNamespacePath` makes the creation namespace of an unqualified relation: the first schema of the search path that exists and that the role may use, where `pg_temp` stands for the session's temporary namespace, which a relation created there creates when it does not exist yet.
pub fn relation_creation_schema(
    state: &dyn RelationCandidateState,
    privileges: &SchemaPrivilegeInquiry<'_>,
    current_user: &(impl crate::catalog::roles::identity::RoleSubject + ?Sized),
) -> Option<String> {
    let temporary = state.temporary_schema_name();
    let search_path = state.search_path().clone();
    search_path.into_iter().find_map(|schema| {
        if schema == "pg_temp" {
            return Some(temporary.clone());
        }
        (privileges.schema_security_for_privilege(&schema).is_some()
            && privileges.schema_has_privilege_for_role(
                &schema,
                current_user,
                SchemaAclPrivilege::Usage,
            ))
        .then_some(schema)
    })
}

/// `RangeVarAdjustRelationPersistence`: the persistence a new relation takes in `schema`. A relation in the session's temporary namespace or its TOAST namespace is temporary; a temporary relation belongs in no other namespace, and an unlogged one in no temporary namespace.
pub fn adjusted_relation_persistence(
    schema: &str,
    persistence: RelationPersistence,
    temporary_schema: &str,
) -> Result<RelationPersistence, SQLError> {
    let own = schema == temporary_schema || schema == temporary_toast_schema_name(temporary_schema);
    let any = own || is_temporary_schema_name(schema);
    let invalid = |message: &str| {
        Err(SQLError::Routine {
            sqlstate: "42P16".into(),
            message: message.into(),
        })
    };
    match persistence {
        RelationPersistence::Temporary | RelationPersistence::Permanent if own => {
            Ok(RelationPersistence::Temporary)
        }
        RelationPersistence::Temporary | RelationPersistence::Permanent if any => {
            invalid("cannot create relations in temporary schemas of other sessions")
        }
        RelationPersistence::Temporary => {
            invalid("cannot create temporary relation in non-temporary schema")
        }
        RelationPersistence::Unlogged if any => {
            invalid("only temporary relations may be created in temporary schemas")
        }
        persistence => Ok(persistence),
    }
}

/// `heap_create`'s refusal of a new relation in `pg_catalog`, `pg_toast` or the session's temporary TOAST namespace, which only system catalog modifications may create relations in.
pub fn ensure_relation_namespace_writable(
    relation: &RelationIdentity,
    temporary_schema: &str,
) -> Result<(), SQLError> {
    let schema = relation.schema.as_str();
    if schema == "pg_catalog"
        || schema == "pg_toast"
        || schema == temporary_toast_schema_name(temporary_schema)
    {
        return Err(SQLError::Diagnostic {
            sqlstate: "42501".into(),
            message: format!("permission denied to create \"{schema}.{}\"", relation.name),
            detail: Some("System catalog modifications are currently disallowed.".into()),
            hint: None,
        });
    }
    Ok(())
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
    let current_user = names.current_role();
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
            let current_user = names.current_role();
            privileges.require_schema_privilege(
                schema,
                &current_user,
                SchemaAclPrivilege::Usage,
            )?;
        }
    }
    let current_user = names.current_role();
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
        // `table_open` refuses indexes and composite types before `DefineIndex` names the relations it cannot index.
        let unopenable = if catalog.indexes().contains(&relation) {
            Some("indexes")
        } else if catalog.composite_types().contains(&relation) {
            Some("composite types")
        } else {
            None
        };
        if let Some(kinds) = unopenable {
            return Err(crate::catalog::analysis::UnopenableRelation {
                name: relation.name,
                kinds,
            }
            .error());
        }
        let unindexable = if catalog.sequences().contains(&relation) {
            Some("sequence")
        } else if catalog.foreign_tables().contains(&relation) {
            Some("foreign table")
        } else {
            None
        };
        if let Some(kind) = unindexable {
            return Err(SQLError::Diagnostic {
                sqlstate: "42809".into(),
                message: format!("cannot create index on relation \"{}\"", relation.name),
                detail: crate::catalog::analysis::relkind_not_supported_detail(kind),
                hint: None,
            });
        }
        if catalog.views().contains(&relation) {
            return Ok(None);
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
