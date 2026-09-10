//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind output sinks to an opaque read generation before physical row delivery.
use crate::query::consumer as physical;
pub use physical::QueryConsumerControl;
use std::{cell::Cell, rc::Rc};
use uqa_core::Value;
use uqa_sql::{type_resolution::coerce_common_context_value, SQLError};

/// A row sink chooses its own services for the selected generation; query execution supplies no writable context.
pub trait QueryConsumerFactory<'consumer, S: Clone + 'static> {
    fn bind(
        self: Rc<Self>,
        generation: Option<&S>,
    ) -> Result<Rc<dyn physical::QueryRowConsumer + 'consumer>, SQLError>;
    fn uses_directional_scan(&self) -> bool {
        false
    }
}

pub(super) struct SetOperationConsumerFactory<'consumer, S: Clone + 'static> {
    downstream: Rc<dyn QueryConsumerFactory<'consumer, S> + 'consumer>,
    state: Rc<SetOperationState>,
}
struct SetOperationState {
    columns: Vec<String>,
    schema: crate::RowSchema,
    offset: Cell<u64>,
    remaining: Cell<Option<u64>>,
    begun: Cell<bool>,
    stopped: Cell<bool>,
}
impl<'consumer, S: Clone + 'static> SetOperationConsumerFactory<'consumer, S> {
    pub(super) fn new(
        downstream: Rc<dyn QueryConsumerFactory<'consumer, S> + 'consumer>,
        schema: crate::RowSchema,
        offset: u64,
        limit: Option<u64>,
    ) -> Self {
        Self {
            downstream,
            state: Rc::new(SetOperationState {
                columns: schema.columns().to_vec(),
                schema,
                offset: Cell::new(offset),
                remaining: Cell::new(limit),
                begun: Cell::new(false),
                stopped: Cell::new(limit == Some(0)),
            }),
        }
    }
    pub(super) fn stopped(&self) -> bool {
        self.state.stopped.get()
    }
}
impl<'consumer, S: Clone + 'static> QueryConsumerFactory<'consumer, S>
    for SetOperationConsumerFactory<'consumer, S>
{
    fn bind(
        self: Rc<Self>,
        generation: Option<&S>,
    ) -> Result<Rc<dyn physical::QueryRowConsumer + 'consumer>, SQLError> {
        Ok(Rc::new(SetOperationRowConsumer {
            downstream: Rc::clone(&self.downstream).bind(generation)?,
            state: Rc::clone(&self.state),
        }))
    }
}
struct SetOperationRowConsumer<'consumer> {
    downstream: Rc<dyn physical::QueryRowConsumer + 'consumer>,
    state: Rc<SetOperationState>,
}
impl SetOperationRowConsumer<'_> {
    fn stopped(&self) -> bool {
        self.state.stopped.get()
    }
}
impl physical::QueryRowConsumer for SetOperationRowConsumer<'_> {
    fn begin(&self, columns: &[String], _schema: &crate::RowSchema) -> Result<(), SQLError> {
        if columns.len() != self.state.columns.len() {
            return Err(SQLError::TypeMismatch(format!(
                "set-operation input width {} does not match output width {}",
                columns.len(),
                self.state.columns.len()
            )));
        }
        if self.state.begun.replace(true) {
            Ok(())
        } else {
            self.downstream
                .begin(&self.state.columns, &self.state.schema)
        }
    }

    fn consume(&self, row: crate::OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
        if self.stopped() {
            return Ok(QueryConsumerControl::Stop);
        }
        if self.state.offset.get() > 0 {
            self.state.offset.set(self.state.offset.get() - 1);
            return Ok(QueryConsumerControl::Continue);
        }
        if self.state.remaining.get() == Some(0) {
            self.state.stopped.set(true);
            return Ok(QueryConsumerControl::Stop);
        }
        let projections = {
            let view = row.view();
            self.state
                .schema
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
        let control = self.downstream.consume(crate::OwnedPhysicalRow::new(
            self.state.schema.clone(),
            row.row
                .project_with_values(projections)
                .without_lock_origins(),
        ))?;
        match control {
            QueryConsumerControl::Continue => {}
            QueryConsumerControl::Stop => {
                self.state.stopped.set(true);
                return Ok(control);
            }
            QueryConsumerControl::Rewind => {
                return Err(SQLError::Internal(
                    "set-operation consumer received a directional rewind".into(),
                ));
            }
        }
        if let Some(remaining) = self.state.remaining.get() {
            let remaining = remaining - 1;
            self.state.remaining.set(Some(remaining));
            if remaining == 0 {
                self.state.stopped.set(true);
                return Ok(QueryConsumerControl::Stop);
            }
        }
        Ok(QueryConsumerControl::Continue)
    }
}

#[derive(Clone)]
pub enum QueryOutputMode<'consumer, S: Clone + 'static> {
    Rows,
    SharedSpill,
    ExistsKeySet,
    RowConsumer(Rc<dyn QueryConsumerFactory<'consumer, S> + 'consumer>),
}
impl<'consumer, S: Clone + 'static> QueryOutputMode<'consumer, S> {
    pub fn physical_consumer(consumer: Rc<dyn physical::QueryRowConsumer + 'consumer>) -> Self {
        Self::RowConsumer(Rc::new(PhysicalConsumer(consumer)))
    }
}
struct PhysicalConsumer<'consumer>(Rc<dyn physical::QueryRowConsumer + 'consumer>);
impl<'consumer, S: Clone + 'static> QueryConsumerFactory<'consumer, S>
    for PhysicalConsumer<'consumer>
{
    fn bind(
        self: Rc<Self>,
        _generation: Option<&S>,
    ) -> Result<Rc<dyn physical::QueryRowConsumer + 'consumer>, SQLError> {
        Ok(Rc::clone(&self.0))
    }
    fn uses_directional_scan(&self) -> bool {
        self.0.uses_directional_scan()
    }
}
