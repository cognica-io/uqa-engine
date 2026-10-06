//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar and grouped assignments keep at most one row and reject the next row immediately.

use super::SubqueryContext;
use crate::query::{
    consumer::{QueryConsumerControl, QueryRowConsumer},
    statement::{consumer::QueryOutputMode, execute_query_plan_output},
};
use crate::scalar::plan::PhysicalOuterRow;
use crate::{OwnedPhysicalRow, RowSchema};
use std::{cell::RefCell, rc::Rc};
use uqa_sql::{plan::QueryPlan, SQLError, SQLParam};

#[derive(Default)]
struct SingleRowConsumer(RefCell<Option<OwnedPhysicalRow>>);

impl QueryRowConsumer for SingleRowConsumer {
    fn begin(&self, _columns: &[String], _schema: &RowSchema) -> Result<(), SQLError> {
        Ok(())
    }
    fn consume(&self, row: OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
        let mut first = self.0.borrow_mut();
        if first.is_some() {
            return Err(crate::scalar::single_row_cardinality_error());
        }
        *first = Some(row);
        Ok(QueryConsumerControl::Continue)
    }
}

pub(super) fn scalar_value(row: Option<OwnedPhysicalRow>) -> uqa_core::Value {
    row.and_then(|row| row.view().value_at(0).cloned())
        .unwrap_or(uqa_core::Value::Null)
}

impl<S: Clone + Send + Sync + 'static> SubqueryContext<'_, S> {
    pub(super) fn execute_correlated_single_row(
        &self,
        plan: &QueryPlan,
        outer: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<Option<OwnedPhysicalRow>, SQLError> {
        if !outer.is_some() {
            return Err(SQLError::Internal(
                "correlated subquery reached execution without a positional outer row".into(),
            ));
        }
        self.execute_single_row_subquery(plan, outer, params)
    }

    pub(super) fn execute_single_row_subquery(
        &self,
        plan: &QueryPlan,
        outer: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<Option<OwnedPhysicalRow>, SQLError> {
        let consumer = Rc::new(SingleRowConsumer::default());
        let mut scope = self.ctes.clone();
        scope.lock_identities.emit = false;
        // Stop after the second output without projecting a vectorized batch ahead of the consumer.
        scope.enable_command_progress_streaming();
        match outer {
            PhysicalOuterRow::Absent => {
                scope.clear_row_lock_outer_row();
                execute_query_plan_output(
                    &self.services.queries.query_context(),
                    plan,
                    params,
                    &mut scope,
                    QueryOutputMode::physical_consumer(consumer.clone()),
                )?;
            }
            PhysicalOuterRow::Physical { schema, row } => {
                let row = OwnedPhysicalRow::new(schema.clone(), row.clone());
                crate::query::sources::lateral_query::execute_lateral_subquery_with_output(
                    &self.services.queries.source_context(),
                    plan,
                    &row,
                    params,
                    &scope,
                    crate::query::consumer::QueryOutputMode::RowConsumer(consumer.clone()),
                )?;
            }
        }
        Ok(consumer.0.take())
    }
}
