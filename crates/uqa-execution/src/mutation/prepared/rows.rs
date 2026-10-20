//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Common spill rows for evaluated mutation actions, independent of their SQL producer.

use crate::{OwnedPhysicalRow, PhysicalRow, RowSchema};
use uqa_sql::SQLError;

use super::{
    decode_prepared_mutation_action, encode_prepared_mutation_action, PreparedMutationAction,
};

const ACTION_WIDTH: usize = 1;

pub fn prepared_mutation_action_schema() -> RowSchema {
    RowSchema::with_internal_relation_types(
        uqa_sql::ast::InternalRelationId::allocate(),
        vec![None],
    )
}

pub fn push_prepared_mutation_action(
    buffer: &mut crate::SpillBuffer,
    schema: &RowSchema,
    action: PreparedMutationAction,
) -> Result<(), SQLError> {
    buffer
        .push(crate::Batch::from_physical_rows(
            schema.clone(),
            vec![PhysicalRow::from_values(vec![
                encode_prepared_mutation_action(action),
            ])],
        ))
        .map_err(crate::physical::physical_exec_error)?;
    Ok(())
}

pub fn decode_prepared_mutation_action_row(
    row: OwnedPhysicalRow,
) -> Result<PreparedMutationAction, SQLError> {
    if row.schema.physical_width() != ACTION_WIDTH {
        return Err(SQLError::Internal(format!(
            "prepared mutation action spill has {} fields, expected {ACTION_WIDTH}",
            row.schema.physical_width()
        )));
    }
    let payload = row.physical_value_at(0).cloned().ok_or_else(|| {
        SQLError::Internal("prepared mutation action spill lost its payload".into())
    })?;
    decode_prepared_mutation_action(payload)
}
