//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::PreparedMutationAction;
use crate::Engine;
pub(in crate::sql) use uqa_execution::mutation::publication::MutationPublicationBatch;
pub(crate) use uqa_execution::row_locks::publication::TransactionRowChange;
use uqa_sql::SQLError;
pub(in crate::sql) fn publish_prepared_mutation_action(
    engine: &Engine,
    action: PreparedMutationAction,
    insert_known_new: bool,
    batch: &mut MutationPublicationBatch,
) -> Result<(), SQLError> {
    uqa_execution::mutation::publication::publish_prepared_mutation_action(
        engine.mutation_publication_context(),
        action,
        insert_known_new,
        batch,
    )
}
pub(in crate::sql) fn finish_mutation_publication(
    engine: &Engine,
    batch: &mut MutationPublicationBatch,
) -> Result<(), SQLError> {
    uqa_execution::mutation::publication::finish_mutation_publication(
        engine.mutation_publication_context(),
        batch,
    )
}
