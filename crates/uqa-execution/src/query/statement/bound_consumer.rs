//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind a row consumer after command CTEs select the statement generation.

use super::{
    consumer::{QueryConsumerControl, QueryOutputMode, QueryRowConsumer},
    context::StatementContext,
};
use crate::query::consumer as physical;
use crate::{BackwardScanSupport, OwnedPhysicalRow, PhysicalScanDirection, RowSchema};
use std::rc::Rc;
use uqa_sql::SQLError;

/// Bind after snapshot selection so INSERT SELECT observes the active statement catalog and mutation visibility for every delivered row.
struct BoundConsumer<'a, S: Clone + 'static> {
    context: StatementContext<'a, S>,
    consumer: Rc<dyn QueryRowConsumer<S>>,
}
impl<S: Clone + 'static> physical::QueryRowConsumer for BoundConsumer<'_, S> {
    fn begin(&self, columns: &[String], schema: &RowSchema) -> Result<(), SQLError> {
        self.consumer.begin(&self.context, columns, schema)
    }
    fn consume(&self, row: OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
        self.consumer.consume(&self.context, row)
    }
    fn uses_directional_scan(&self) -> bool {
        self.consumer.uses_directional_scan()
    }
    fn directional_scan_prepared(&self, support: BackwardScanSupport) -> Result<(), SQLError> {
        self.consumer
            .directional_scan_prepared(&self.context, support)
    }
    fn scan_direction(&self) -> PhysicalScanDirection {
        self.consumer.scan_direction()
    }
    fn direction_exhausted(&self) -> Result<QueryConsumerControl, SQLError> {
        self.consumer.direction_exhausted(&self.context)
    }
    fn rewound(&self) -> Result<QueryConsumerControl, SQLError> {
        self.consumer.rewound(&self.context)
    }
}

pub(super) fn bind_output_mode<'a, S: Clone + 'static>(
    context: &StatementContext<'a, S>,
    mode: QueryOutputMode<S>,
) -> physical::QueryOutputMode<'a> {
    match mode {
        QueryOutputMode::Rows => physical::QueryOutputMode::Rows,
        QueryOutputMode::SharedSpill => physical::QueryOutputMode::SharedSpill,
        QueryOutputMode::ExistsKeySet => physical::QueryOutputMode::ExistsKeySet,
        QueryOutputMode::RowConsumer(consumer) => {
            physical::QueryOutputMode::RowConsumer(Rc::new(BoundConsumer {
                context: *context,
                consumer,
            }))
        }
    }
}
