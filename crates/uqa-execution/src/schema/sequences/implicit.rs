//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialize implicit sequence declarations before publishing their table columns.
use super::creation::SequenceCreationNamespace;
use uqa_core::RelationIdentity;
use uqa_sql::schema::sequences::implicit::{
    apply_implicit_sequence_metadata, choose_implicit_sequence_name, implicit_sequence_data_type,
};
use uqa_sql::{
    ast::{RelationPersistence, SequenceDataType},
    SQLError,
};
use uqa_storage::StorageBackendError;

pub trait ImplicitSequencePublication {
    fn create_implicit_sequence(
        &self,
        name: &str,
        start: i64,
        increment: i64,
        data_type: SequenceDataType,
        persistence: RelationPersistence,
    ) -> Result<(), SQLError>;
}

pub struct ImplicitSequenceContext<'a> {
    pub namespace: &'a dyn SequenceCreationNamespace,
    pub publication: &'a dyn ImplicitSequencePublication,
}

pub fn materialize_implicit_sequences(
    context: &ImplicitSequenceContext<'_>,
    statement: &str,
    table_name: &str,
    columns: &mut [uqa_sql::ast::ColumnDef],
    persistence: uqa_sql::ast::RelationPersistence,
) -> Result<(), SQLError> {
    let relation = RelationIdentity::from_legacy_name(table_name)
        .map_err(|error| SQLError::Internal(format!("resolve {statement} relation: {error}")))?;
    let mut sequences = Vec::new();
    for (column_index, column) in columns.iter().enumerate() {
        let Some(auto_increment) = column.auto_increment.as_ref() else {
            continue;
        };
        if auto_increment.kind == uqa_sql::ast::AutoIncrementKind::Legacy
            || auto_increment.sequence.is_some()
        {
            continue;
        }
        let data_type = implicit_sequence_data_type(column).map_err(SQLError::Internal)?;
        let sequence = choose_implicit_sequence_name(
            &relation,
            &column.name,
            |candidate| {
                context
                    .namespace
                    .relation_exists(&candidate.qualified_name())
            },
            StorageBackendError::Other,
        )
        .map_err(|error| {
            SQLError::Internal(format!(
                "choose implicit sequence for `{table_name}`.`{}`: {error}",
                column.name
            ))
        })?;
        sequences.push((column_index, data_type, sequence));
    }
    for (column_index, data_type, sequence) in sequences {
        context
            .publication
            .create_implicit_sequence(&sequence, 1, 1, data_type, persistence)?;
        apply_implicit_sequence_metadata(table_name, &mut columns[column_index], sequence)
            .map_err(SQLError::Internal)?;
    }
    Ok(())
}
