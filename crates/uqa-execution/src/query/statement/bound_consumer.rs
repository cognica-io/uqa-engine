//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select the output sink once after CTEs determine the read generation.
use super::consumer::QueryOutputMode;
use crate::query::consumer as physical;
use uqa_sql::SQLError;

pub(super) fn bind_output_mode<'consumer, S: Clone + 'static>(
    generation: Option<&S>,
    mode: QueryOutputMode<'consumer, S>,
) -> Result<physical::QueryOutputMode<'consumer>, SQLError> {
    Ok(match mode {
        QueryOutputMode::Rows => physical::QueryOutputMode::Rows,
        QueryOutputMode::SharedSpill => physical::QueryOutputMode::SharedSpill,
        QueryOutputMode::ExistsKeySet => physical::QueryOutputMode::ExistsKeySet,
        QueryOutputMode::RowConsumer(consumer) => {
            physical::QueryOutputMode::RowConsumer(consumer.bind(generation)?)
        }
    })
}
