//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One qualified deletion completes its RETURNING observation before the next row.

use crate::mutation::{
    candidate::MutationCandidate,
    events::MutationEventQueue,
    prepared::{
        prepared_mutation_action_schema, push_prepared_mutation_action, PreparedMutationAction,
    },
    returning::{ReturningProjectionRow, ReturningRows},
    row_images::{MutationRowImage, MutationRowImages},
    row_membership::PhysicalRowLookup,
    statement::context::MutationStatementContext,
};
use crate::{query::CteScope, RowSchema, SpillBuffer};
use uqa_sql::{plan::DeletePlan, SQLError, SQLParam};

pub(super) struct DeleteRows {
    pub returning: ReturningRows,
    pub prepared: SpillBuffer,
    pub events: MutationEventQueue,
    schema: RowSchema,
}

impl DeleteRows {
    pub fn new<S: Clone + Send + Sync + 'static>(
        context: &MutationStatementContext<'_, S>,
    ) -> Result<Self, SQLError> {
        Ok(Self {
            returning: ReturningRows::new(context.query.source.relational.runtime)?,
            prepared: SpillBuffer::new(context.mutation.rules.rules.memory.work_mem_bytes()?),
            events: MutationEventQueue::default(),
            schema: prepared_mutation_action_schema(),
        })
    }

    pub fn prepare<S: Clone + Send + Sync + 'static>(
        &mut self,
        context: &MutationStatementContext<'_, S>,
        stmt: &DeletePlan,
        params: &[SQLParam],
        ctes: &CteScope<S>,
        candidate: MutationCandidate<Option<crate::OwnedPhysicalRow>>,
        root_deletes: &PhysicalRowLookup<'_>,
    ) -> Result<bool, SQLError> {
        let Some(prepared) = crate::mutation::referential::prepare_document_delete_excluding(
            &context.mutation.preparation.referential,
            &candidate.identity.table,
            candidate.identity.doc_id,
            root_deletes,
            false,
        )?
        else {
            return Ok(false);
        };
        let old_metadata = crate::mutation::rows::existing_tuple_metadata(
            context.mutation.preparation.referential.assignment.rows,
            &prepared.table,
            prepared.doc_id,
        )?;
        crate::mutation::staging::stage_prepared_document_delete(
            context.mutation.preparation.staging,
            &prepared,
            self.events.after_rows_mut(),
        )?;
        if !stmt.returning.is_empty() {
            self.returning.push_to(
                crate::mutation::returning::build_returning_row(
                    context.mutation.preparation.returning,
                    ReturningProjectionRow {
                        table: &stmt.table,
                        target_qualifier: &stmt.target_qualifier,
                        images: MutationRowImages {
                            old: Some(MutationRowImage {
                                storage_table: prepared.table.clone(),
                                doc_id: prepared.doc_id,
                                document: &prepared.document,
                                metadata: old_metadata,
                            }),
                            new: None,
                        },
                        aliases: &stmt.returning_aliases,
                        context: candidate.context.as_ref(),
                    },
                    &stmt.returning,
                    params,
                    ctes,
                )?,
                context.mutation.preparation.returning.destination,
            )?;
        }
        context
            .query
            .source
            .relational
            .runtime
            .cancellation_token()
            .check()?;
        push_prepared_mutation_action(
            &mut self.prepared,
            &self.schema,
            PreparedMutationAction::Delete(prepared),
        )?;
        Ok(true)
    }
}
