//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate native catalog relationships without invoking SQL or reconstructing stored definitions.

use super::{CatalogDefinitionSnapshot, CatalogReadSnapshot};
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::{catalog::security::authority::AuthorityCatalog, SQLError};

mod type_dependencies;
pub use type_dependencies::validate as validate_types;
mod plan_dependencies;
pub use plan_dependencies::validate as validate_plan_relations;
mod routine_dependencies;
pub use routine_dependencies::validate as validate_routines;
mod sequence_functions;
pub use sequence_functions::validate as validate_sequence_functions;
mod namespaces;
pub use namespaces::validate as validate_namespaces;
mod name_claims;
pub use name_claims::validate as validate_name_uniqueness;

/// Join complete immutable ordinary/foreign row layouts and original index rows for SQL's constraint-reference predicate.
pub fn validate_constraints(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    use uqa_sql::schema::constraints::native::NativeConstraintTable;
    uqa_sql::schema::constraints::native::validate(
        snapshot
            .tables
            .iter()
            .map(|(name, table)| NativeConstraintTable {
                name,
                object_id: table.object_id,
                columns: &table.columns,
                checks: &table.checks,
                keys: &table.keys,
                foreign_keys: &table.foreign_keys,
                hierarchy: Some(&table.hierarchy),
            })
            .chain(
                snapshot
                    .definitions
                    .foreign_tables
                    .iter()
                    .map(|(name, table)| NativeConstraintTable {
                        name,
                        object_id: table.object_id,
                        columns: &table.columns,
                        checks: &table.checks,
                        keys: &[],
                        foreign_keys: &[],
                        hierarchy: None,
                    }),
            ),
        snapshot.definitions.catalog_indexes.values(),
        memory,
        cancellation,
    )
}

/// Join original ordinary-table and index membership for SQL's native ownership predicate.
pub fn validate_indexes(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    uqa_sql::catalog::index::native::validate(
        snapshot.tables.iter().map(|(name, table)| {
            uqa_sql::catalog::index::native::NativeIndexTable {
                name,
                object_id: table.object_id,
                keys: &table.keys,
                hierarchy: &table.hierarchy,
            }
        }),
        snapshot.definitions.catalog_indexes.values(),
        memory,
        cancellation,
    )
}

/// Join ordinary and foreign column identities with the selected native sequence registry; SQL checks the original dependency forms without migration or value allocation.
pub fn validate_sequences(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    uqa_sql::schema::sequences::native::validate(
        snapshot
            .tables
            .values()
            .map(|table| (table.object_id, table.columns.as_slice()))
            .chain(
                snapshot
                    .definitions
                    .foreign_tables
                    .values()
                    .map(|table| (table.object_id, table.columns.as_slice())),
            ),
        snapshot
            .definitions
            .sequences
            .iter()
            .map(|(name, record)| (name, record.state.owner)),
        memory,
        cancellation,
    )
}

/// Validate original table parent references against the complete selected table registry. SQL owns inheritance/partition semantics and the admitted acyclicity traversal.
pub fn validate_hierarchies(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    uqa_sql::schema::inheritance::native::validate(
        snapshot
            .tables
            .iter()
            .map(|(name, table)| (name, &*table.hierarchy, table.persistence)),
        memory,
        cancellation,
    )
}

#[cfg(test)]
mod tests;

/// Authority closure for the supplied immutable catalog: every owner, ACL endpoint and membership grantor names the original live role incarnation, deleted membership targets retain their old identities, every ACL grant is rooted at its owner, and column authority names an existing public attribute. Structural native codecs remain responsible for their own tuple formats. Other catalog dependencies are validated separately before activation.
pub fn validate_authority(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    let definitions = &snapshot.definitions;
    let authority = AuthorityCatalog::new(&definitions.roles, memory, cancellation)?;
    authority.memberships(&definitions.role_memberships)?;
    authority.database(&definitions.database_security)?;
    for schema in definitions.schemas.values() {
        authority.schema(schema)?;
    }
    for (_, table) in &snapshot.tables {
        authority.table(&table.security, |name| {
            has_column(
                table.columns.iter().map(|column| column.name.as_str()),
                name,
                cancellation,
            )
        })?;
    }
    relations(definitions, &authority, cancellation)?;
    types(definitions, &authority)?;
    for overloads in definitions.sql_user_functions.values() {
        for routine in overloads {
            authority.object(
                routine.def.owner.ok_or_else(|| invalid("routine owner"))?,
                routine.def.execute_acl.as_deref(),
            )?;
        }
    }
    for security in definitions.builtin_routine_security.values() {
        authority.object(
            uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP,
            Some(&security.execute_acl),
        )?;
    }
    for (name, security) in definitions.system_relation_security.iter() {
        let relation = uqa_sql::catalog::SystemRelation::at(&name.schema, &name.name)
            .ok_or_else(|| invalid("system relation authority"))?;
        authority.system_relation(relation, security)?;
    }
    for wrapper in definitions.foreign_wrappers.values() {
        authority.role(wrapper.owner)?;
    }
    for server in definitions.foreign_servers.values() {
        authority.role(server.metadata.owner)?;
    }
    cancellation.check()?;
    Ok(())
}

fn relations(
    definitions: &CatalogDefinitionSnapshot,
    authority: &AuthorityCatalog<'_>,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    for (_, security) in definitions.sequences.security().iter() {
        authority.sequence(security)?;
    }
    for view in definitions.views.values() {
        authority.table(&view.security, |name| {
            has_column(
                view.output_columns.iter().flatten().map(String::as_str),
                name,
                cancellation,
            )
        })?;
    }
    if definitions.foreign_tables.len() != definitions.foreign_table_security.len() {
        return Err(invalid("foreign relation authority membership"));
    }
    for (name, table) in definitions.foreign_tables.iter() {
        cancellation.check()?;
        let security = definitions
            .foreign_table_security
            .get(name)
            .ok_or_else(|| invalid("foreign relation authority"))?;
        authority.table(security, |name| {
            has_column(
                table.columns.iter().map(|column| column.name.as_str()),
                name,
                cancellation,
            )
        })?;
    }
    Ok(())
}

fn types(
    definitions: &CatalogDefinitionSnapshot,
    authority: &AuthorityCatalog<'_>,
) -> Result<(), SQLError> {
    macro_rules! check {
        ($($field:ident),+ $(,)?) => {$(
            for definition in definitions.$field.values() {
                authority.object(definition.owner, definition.usage_acl.as_deref())?;
            }
        )+};
    }
    check!(domains, enums, composites, ranges, shells);
    Ok(())
}

fn has_column<'a>(
    columns: impl Iterator<Item = &'a str>,
    name: &str,
    cancellation: &CancellationToken,
) -> Result<bool, SQLError> {
    for column in columns {
        cancellation.check()?;
        if column == name {
            return Ok(true);
        }
    }
    Ok(false)
}

fn invalid(kind: &str) -> SQLError {
    SQLError::Internal(format!("invalid native catalog {kind}"))
}
