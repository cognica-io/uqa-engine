//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    DocId, Document, Engine, MutationPublicationBatch, PreparedInsertConflict, SQLError, SQLParam,
};
pub(super) use uqa_execution::mutation::insert::rows::prepare_values_insert_row;
pub(super) fn apply_validated_prepared_insert(
    engine: &Engine,
    table: &str,
    document: Document,
    prepared: PreparedInsertConflict,
    known_new: bool,
    publication: &mut MutationPublicationBatch,
) -> Result<bool, SQLError> {
    uqa_execution::mutation::publication::apply_validated_prepared_insert(
        engine.mutation_publication_context(),
        table,
        document,
        prepared,
        known_new,
        publication,
    )
}
pub(in crate::sql) fn refresh_insert_identity_after_trigger(
    engine: &Engine,
    table: &str,
    id_column: &str,
    accepts_supplied_identity: bool,
    auto_id_column: Option<&str>,
    document: &Document,
    identity: &mut (DocId, bool),
) -> Result<(), SQLError> {
    uqa_execution::mutation::identity::refresh_insert_identity_after_trigger(
        uqa_execution::mutation::identity::IdentityAllocationContext {
            identifiers: engine,
            partitions: engine,
        },
        table,
        id_column,
        accepts_supplied_identity,
        auto_id_column,
        document,
        identity,
    )
}
pub(in crate::sql) fn apply_missing_column_defaults(
    engine: &Engine,
    table: &str,
    document: &mut Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    uqa_execution::mutation::assignment::apply_missing_column_defaults(
        engine.mutation_assignment_context(),
        table,
        document,
        params,
    )
}
