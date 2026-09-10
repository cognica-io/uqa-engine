//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generation-aware row delivery for streaming query consumers.
use super::context::StatementContext;
use std::{cell::Cell, rc::Rc};
use uqa_core::Value;
use uqa_sql::{type_resolution::coerce_common_context_value, SQLError};

pub trait QueryRowConsumer<S: Clone + 'static> {
    fn begin(
        &self,
        context: &StatementContext<'_, S>,
        columns: &[String],
        schema: &crate::RowSchema,
    ) -> Result<(), SQLError>;

    fn consume(
        &self,
        context: &StatementContext<'_, S>,
        row: crate::OwnedPhysicalRow,
    ) -> Result<QueryConsumerControl, SQLError>;

    fn uses_directional_scan(&self) -> bool {
        false
    }

    fn directional_scan_prepared(
        &self,
        _context: &StatementContext<'_, S>,
        _support: crate::BackwardScanSupport,
    ) -> Result<(), SQLError> {
        Ok(())
    }

    fn scan_direction(&self) -> crate::PhysicalScanDirection {
        crate::PhysicalScanDirection::Forward
    }

    fn direction_exhausted(
        &self,
        _context: &StatementContext<'_, S>,
    ) -> Result<QueryConsumerControl, SQLError> {
        Ok(QueryConsumerControl::Stop)
    }

    fn rewound(
        &self,
        _context: &StatementContext<'_, S>,
    ) -> Result<QueryConsumerControl, SQLError> {
        Ok(QueryConsumerControl::Continue)
    }
}

pub use crate::query::consumer::QueryConsumerControl;

pub(super) struct SetOperationRowConsumer<S: Clone + 'static> {
    downstream: Rc<dyn QueryRowConsumer<S>>,
    columns: Vec<String>,
    schema: crate::RowSchema,
    offset: Cell<u64>,
    remaining: Cell<Option<u64>>,
    begun: Cell<bool>,
    stopped: Cell<bool>,
}

impl<S: Clone + 'static> SetOperationRowConsumer<S> {
    pub(super) fn new(
        downstream: Rc<dyn QueryRowConsumer<S>>,
        schema: crate::RowSchema,
        offset: u64,
        limit: Option<u64>,
    ) -> Self {
        Self {
            columns: schema.columns().to_vec(),
            downstream,
            schema,
            offset: Cell::new(offset),
            remaining: Cell::new(limit),
            begun: Cell::new(false),
            stopped: Cell::new(limit == Some(0)),
        }
    }

    pub(super) fn stopped(&self) -> bool {
        self.stopped.get()
    }
}

impl<S: Clone + 'static> QueryRowConsumer<S> for SetOperationRowConsumer<S> {
    fn begin(
        &self,
        context: &StatementContext<'_, S>,
        columns: &[String],
        _schema: &crate::RowSchema,
    ) -> Result<(), SQLError> {
        if columns.len() != self.columns.len() {
            return Err(SQLError::TypeMismatch(format!(
                "set-operation input width {} does not match output width {}",
                columns.len(),
                self.columns.len()
            )));
        }
        if self.begun.replace(true) {
            Ok(())
        } else {
            self.downstream.begin(context, &self.columns, &self.schema)
        }
    }

    fn consume(
        &self,
        context: &StatementContext<'_, S>,
        row: crate::OwnedPhysicalRow,
    ) -> Result<QueryConsumerControl, SQLError> {
        if self.stopped() {
            return Ok(QueryConsumerControl::Stop);
        }
        if self.offset.get() > 0 {
            self.offset.set(self.offset.get() - 1);
            return Ok(QueryConsumerControl::Continue);
        }
        if self.remaining.get() == Some(0) {
            self.stopped.set(true);
            return Ok(QueryConsumerControl::Stop);
        }
        let projections = {
            let view = row.view();
            self.schema
                .column_types()
                .iter()
                .enumerate()
                .map(|(position, target_type)| {
                    let source_type = row.schema.column_type(position);
                    if target_type
                        .as_ref()
                        .is_some_and(|target_type| source_type != Some(target_type))
                    {
                        let value = view.value_at(position).cloned().unwrap_or(Value::Null);
                        return coerce_common_context_value(
                            value,
                            source_type,
                            target_type.as_ref(),
                        )
                        .map(crate::RowProjectionValue::Owned);
                    }
                    Ok(row.schema.physical_slot(position).map_or(
                        crate::RowProjectionValue::Owned(Value::Null),
                        crate::RowProjectionValue::InputSlot,
                    ))
                })
                .collect::<Result<Vec<_>, SQLError>>()?
        };
        let control = self.downstream.consume(
            context,
            crate::OwnedPhysicalRow::new(
                self.schema.clone(),
                row.row
                    .project_with_values(projections)
                    .without_lock_origins(),
            ),
        )?;
        match control {
            QueryConsumerControl::Continue => {}
            QueryConsumerControl::Stop => {
                self.stopped.set(true);
                return Ok(control);
            }
            QueryConsumerControl::Rewind => {
                return Err(SQLError::Internal(
                    "set-operation consumer received a directional rewind".into(),
                ));
            }
        }
        if let Some(remaining) = self.remaining.get() {
            let remaining = remaining - 1;
            self.remaining.set(Some(remaining));
            if remaining == 0 {
                self.stopped.set(true);
                return Ok(QueryConsumerControl::Stop);
            }
        }
        Ok(QueryConsumerControl::Continue)
    }
}

#[derive(Clone)]
pub enum QueryOutputMode<S: Clone + 'static> {
    Rows,
    SharedSpill,
    ExistsKeySet,
    RowConsumer(Rc<dyn QueryRowConsumer<S>>),
}

impl<S: Clone + 'static> QueryOutputMode<S> {
    pub fn physical_consumer(consumer: Rc<dyn crate::query::consumer::QueryRowConsumer>) -> Self {
        Self::RowConsumer(Rc::new(PhysicalConsumer(consumer)))
    }
}
struct PhysicalConsumer(Rc<dyn crate::query::consumer::QueryRowConsumer>);
impl<S: Clone + 'static> QueryRowConsumer<S> for PhysicalConsumer {
    fn begin(
        &self,
        _context: &StatementContext<'_, S>,
        columns: &[String],
        schema: &crate::RowSchema,
    ) -> Result<(), SQLError> {
        self.0.begin(columns, schema)
    }
    fn consume(
        &self,
        _context: &StatementContext<'_, S>,
        row: crate::OwnedPhysicalRow,
    ) -> Result<QueryConsumerControl, SQLError> {
        self.0.consume(row)
    }
    fn uses_directional_scan(&self) -> bool {
        self.0.uses_directional_scan()
    }
    fn directional_scan_prepared(
        &self,
        _context: &StatementContext<'_, S>,
        support: crate::BackwardScanSupport,
    ) -> Result<(), SQLError> {
        self.0.directional_scan_prepared(support)
    }
    fn scan_direction(&self) -> crate::PhysicalScanDirection {
        self.0.scan_direction()
    }
    fn direction_exhausted(
        &self,
        _context: &StatementContext<'_, S>,
    ) -> Result<QueryConsumerControl, SQLError> {
        self.0.direction_exhausted()
    }
    fn rewound(
        &self,
        _context: &StatementContext<'_, S>,
    ) -> Result<QueryConsumerControl, SQLError> {
        self.0.rewound()
    }
}
