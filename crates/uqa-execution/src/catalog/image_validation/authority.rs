//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The immutable catalog supplies rows to SQL's role and privilege validation in publication order.

use crate::catalog::{CatalogDefinitionSnapshot, CatalogReadSnapshot};
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::{
    ast::ObjectAclEntry,
    catalog::{roles::RoleIdentity, security::authority::AuthorityCatalog, CatalogWork},
    SQLError,
};

/// Authority closure for the supplied immutable catalog: every owner, ACL endpoint and membership grantor names the original live role incarnation, deleted membership targets retain their old identities, every ACL grant is rooted at its owner, and column authority names an existing public attribute. Structural native codecs remain responsible for their own tuple formats. Other catalog dependencies are validated separately before activation.
pub fn validate_authority(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    crate::catalog::selection_work::inline(validate(snapshot, memory, cancellation, &mut ()))
}

pub(super) async fn validate(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    work: &mut (impl CatalogWork + Send),
) -> Result<(), SQLError> {
    let definitions = &snapshot.definitions;
    let authority =
        AuthorityCatalog::new_with_work(&definitions.roles, memory, cancellation, work).await?;
    authority
        .memberships_with_work(&definitions.role_memberships, work)
        .await?;
    authority
        .database_with_work(&definitions.database_security, work)
        .await?;
    for schema in definitions.schemas.values() {
        authority.checkpoint(work).await?;
        authority.schema_with_work(schema, work).await?;
    }
    for (_, table) in &snapshot.tables {
        authority.checkpoint(work).await?;
        authority
            .table_with_work(&table.security, table.columns.iter().map(column_name), work)
            .await?;
    }
    for (key, row) in definitions.default_acls.iter() {
        authority.checkpoint(work).await?;
        authority.role(key.owner)?;
        for entry in &row.acl {
            authority.checkpoint(work).await?;
            if let Some(role) = entry.role {
                authority.role(role)?;
            }
        }
    }
    relations(definitions, &authority, work).await?;
    types(definitions, &authority, work).await?;
    routines(definitions, &authority, work).await?;
    for (name, security) in definitions.system_relation_security.iter() {
        authority.checkpoint(work).await?;
        let relation = uqa_sql::catalog::SystemRelation::at(&name.schema, &name.name)
            .ok_or_else(|| invalid("system relation authority"))?;
        authority
            .system_relation_with_work(relation, security, work)
            .await?;
    }
    foreign_owners(definitions, &authority, work).await?;
    cancellation.check()?;
    authority.retire(work).await
}

async fn relations(
    definitions: &CatalogDefinitionSnapshot,
    authority: &AuthorityCatalog<'_>,
    work: &mut (impl CatalogWork + Send),
) -> Result<(), SQLError> {
    for (_, security) in definitions.sequences.security().iter() {
        authority.checkpoint(work).await?;
        authority.sequence_with_work(security, work).await?;
    }
    for view in definitions.views.values() {
        authority.checkpoint(work).await?;
        authority
            .table_with_work(
                &view.security,
                view.output_columns.iter().flatten().map(String::as_str),
                work,
            )
            .await?;
    }
    if definitions.foreign_tables.len() != definitions.foreign_table_security.len() {
        return Err(invalid("foreign relation authority membership"));
    }
    for (name, table) in definitions.foreign_tables.iter() {
        authority.checkpoint(work).await?;
        let security = definitions
            .foreign_table_security
            .get(name)
            .ok_or_else(|| invalid("foreign relation authority"))?;
        authority
            .table_with_work(security, table.columns.iter().map(column_name), work)
            .await?;
    }
    Ok(())
}

async fn routines(
    definitions: &CatalogDefinitionSnapshot,
    authority: &AuthorityCatalog<'_>,
    work: &mut (impl CatalogWork + Send),
) -> Result<(), SQLError> {
    for overloads in definitions.sql_user_functions.values() {
        authority.checkpoint(work).await?;
        for routine in overloads {
            authority.checkpoint(work).await?;
            authority
                .object_with_work(
                    routine.def.owner.ok_or_else(|| invalid("routine owner"))?,
                    routine.def.execute_acl.as_deref(),
                    work,
                )
                .await?;
        }
    }
    for security in definitions.builtin_routine_security.values() {
        authority.checkpoint(work).await?;
        authority
            .object_with_work(RoleIdentity::BOOTSTRAP, Some(&security.execute_acl), work)
            .await?;
    }
    Ok(())
}

async fn foreign_owners(
    definitions: &CatalogDefinitionSnapshot,
    authority: &AuthorityCatalog<'_>,
    work: &mut (impl CatalogWork + Send),
) -> Result<(), SQLError> {
    let wrappers = definitions
        .foreign_wrappers
        .values()
        .map(|wrapper| wrapper.owner);
    let servers = definitions
        .foreign_servers
        .values()
        .map(|server| server.metadata.owner);
    for owner in wrappers.chain(servers) {
        authority.checkpoint(work).await?;
        authority.role(owner)?;
    }
    Ok(())
}

async fn types(
    definitions: &CatalogDefinitionSnapshot,
    authority: &AuthorityCatalog<'_>,
    work: &mut (impl CatalogWork + Send),
) -> Result<(), SQLError> {
    macro_rules! check {
        ($($field:ident),+ $(,)?) => {$(
            type_owners(definitions.$field.values(), authority, work, |definition| {
                (definition.owner, definition.usage_acl.as_deref())
            }).await?;
        )+};
    }
    check!(domains, enums, composites, ranges, shells);
    Ok(())
}

async fn type_owners<'a, T: Sync + 'a>(
    definitions: impl Iterator<Item = &'a T> + Send,
    authority: &AuthorityCatalog<'_>,
    work: &mut (impl CatalogWork + Send),
    security: fn(&T) -> (RoleIdentity, Option<&[ObjectAclEntry]>),
) -> Result<(), SQLError> {
    for definition in definitions {
        authority.checkpoint(work).await?;
        let (owner, acl) = security(definition);
        authority.object_with_work(owner, acl, work).await?;
    }
    Ok(())
}

fn column_name(column: &uqa_sql::ast::ColumnDef) -> &str {
    &column.name
}

fn invalid(kind: &str) -> SQLError {
    SQLError::Internal(format!("invalid native catalog {kind}"))
}
