//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialize implicit sequence declarations before publishing their table columns.
use super::creation::SequenceCreationNamespace;
use crate::catalog::sequence::SequenceState;
use uqa_core::RelationIdentity;
use uqa_sql::ast::{
    AutoIncrementKind, ColumnDef, IdentitySequenceDeclaration, RelationPersistence,
};
use uqa_sql::schema::sequences::{
    declaration::{declare_sequence, sequence_ownership},
    implicit::{apply_implicit_sequence_metadata, choose_implicit_sequence_name},
    ownership::{bind_sequence_owner, SequenceOwnerCatalog},
};
use uqa_sql::SQLError;
use uqa_storage::StorageBackendError;

pub trait ImplicitSequencePublication {
    fn create_implicit_sequence(
        &self,
        name: &str,
        state: SequenceState,
        persistence: RelationPersistence,
    ) -> Result<(), SQLError>;
}

pub struct ImplicitSequenceContext<'a> {
    pub namespace: &'a dyn SequenceCreationNamespace,
    pub owners: &'a dyn SequenceOwnerCatalog,
    pub publication: &'a dyn ImplicitSequencePublication,
}

/// Create the sequence of each `SERIAL` and identity column of `table_name`, from the options an identity declaration writes, and record it on its column. `PostgreSQL` names every sequence while it analyzes the statement, and then creates them in column order before the table.
pub fn materialize_implicit_sequences(
    context: &ImplicitSequenceContext<'_>,
    statement: &str,
    table_name: &str,
    columns: &mut [ColumnDef],
    persistence: RelationPersistence,
) -> Result<(), SQLError> {
    let relation = RelationIdentity::from_legacy_name(table_name)
        .map_err(|error| SQLError::Internal(format!("resolve {statement} relation: {error}")))?;
    let mut sequences = Vec::new();
    for (index, column) in columns.iter_mut().enumerate() {
        let Some(auto_increment) = column.auto_increment.as_mut() else {
            continue;
        };
        let declaration = auto_increment
            .declaration
            .take()
            .map(|declaration| *declaration)
            .unwrap_or_default();
        if auto_increment.kind == AutoIncrementKind::Legacy || auto_increment.sequence.is_some() {
            continue;
        }
        let identity = auto_increment.is_identity();
        let name = sequence_name(context, &relation, &column.name, &declaration)?;
        let persistence = sequence_persistence(&declaration, persistence)?;
        sequences.push((index, name, persistence, declaration, identity));
    }
    for (index, name, persistence, declaration, identity) in sequences {
        let column = &mut columns[index];
        create_column_sequence(
            context,
            &relation,
            column,
            &name,
            persistence,
            &declaration,
            identity,
        )?;
        apply_implicit_sequence_metadata(table_name, column, name).map_err(SQLError::Internal)?;
    }
    Ok(())
}

/// The name `SEQUENCE NAME` gives a column's sequence, in its table's schema unless it names another, or the free name `PostgreSQL` chooses from the table and column names.
fn sequence_name(
    context: &ImplicitSequenceContext<'_>,
    relation: &RelationIdentity,
    column: &str,
    declaration: &IdentitySequenceDeclaration,
) -> Result<String, SQLError> {
    if let Some(name) = &declaration.name {
        return Ok(RelationIdentity::new(
            name.schema
                .clone()
                .unwrap_or_else(|| relation.schema.clone()),
            name.name.clone(),
        )
        .qualified_name());
    }
    choose_implicit_sequence_name(
        relation,
        column,
        |candidate| {
            context
                .namespace
                .relation_exists(&candidate.qualified_name())
        },
        StorageBackendError::Other,
    )
    .map_err(|error| {
        SQLError::Internal(format!(
            "choose implicit sequence for `{}`.`{column}`: {error}",
            relation.qualified_name()
        ))
    })
}

/// A column sequence takes its table's persistence, or the `LOGGED` or `UNLOGGED` its declaration writes, which a temporary table's sequence cannot take.
fn sequence_persistence(
    declaration: &IdentitySequenceDeclaration,
    table: RelationPersistence,
) -> Result<RelationPersistence, SQLError> {
    match declaration.persistence {
        Some(_) if table == RelationPersistence::Temporary => Err(SQLError::Routine {
            sqlstate: "42P16".into(),
            message: "cannot set logged status of a temporary sequence".into(),
        }),
        Some(declared) => Ok(declared),
        None => Ok(table),
    }
}

/// Create one column's sequence as the `CREATE SEQUENCE` `PostgreSQL` issues for it does: read its options, create it, and read the `OWNED BY` the declaration writes.
fn create_column_sequence(
    context: &ImplicitSequenceContext<'_>,
    relation: &RelationIdentity,
    column: &ColumnDef,
    name: &str,
    persistence: RelationPersistence,
    declaration: &IdentitySequenceDeclaration,
    identity: bool,
) -> Result<(), SQLError> {
    if let Some(error) = declaration.error.clone() {
        return Err(error.into());
    }
    let declared = declare_sequence(&declaration.sequence, &column.ty, identity)?;
    let mut state = SequenceState::from_definition(declared.definition);
    state.current = declared.current;
    context
        .publication
        .create_implicit_sequence(name, state, persistence)?;
    // The column itself owns the sequence. An `OWNED BY` the declaration writes is read and checked as `CREATE SEQUENCE` reads it, and then replaced.
    if let Some(names) = declaration.sequence.owned_by.as_deref() {
        bind_sequence_owner(context.owners, name, &sequence_ownership(names)?)?;
    }
    // `PostgreSQL` links the sequence to its column by naming the table in the sequence's schema.
    if let Some(schema) = declaration
        .name
        .as_ref()
        .and_then(|name| name.schema.as_ref())
        .filter(|schema| **schema != relation.schema)
    {
        let linked = format!("{schema}.{}", relation.name);
        if !context
            .namespace
            .relation_exists(&linked)
            .map_err(|error| SQLError::Internal(format!("resolve relation `{linked}`: {error}")))?
        {
            return Err(SQLError::Routine {
                sqlstate: "42P01".into(),
                message: format!("relation \"{linked}\" does not exist"),
            });
        }
    }
    Ok(())
}
