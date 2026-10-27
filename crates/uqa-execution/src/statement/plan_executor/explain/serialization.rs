//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Measure actual typed output conversion without retaining a second query result.

use crate::query::diagnostics::io::Operation;
use crate::{RowSchema, RowSchemaExecution};
use std::{
    cell::{Cell, RefCell},
    sync::Arc,
    time::Instant,
};
use uqa_core::{
    io_usage::{IOContext, IOUsage},
    memory::{Produced, ProductionControl},
};
use uqa_sql::{
    explain::ExplainSerialization,
    result::{
        encode_postgres_row_with_control, explain::ExplainSerializationMetrics, ResultFormat,
    },
    SQLError,
};

pub(super) struct RowSerialization<'a> {
    format: ResultFormat,
    timing: bool,
    engine: Arc<dyn uqa_sql::expr::EngineHook + 'a>,
    retained: &'a uqa_storage::read_control::StorageReadControl,
    invoking: &'a uqa_core::CancellationToken,
    positions: RefCell<Option<Produced<Vec<Option<usize>>>>>,
    metrics: Cell<ExplainSerializationMetrics>,
}

impl<'a> RowSerialization<'a> {
    pub(super) fn new(
        format: ExplainSerialization,
        timing: bool,
        engine: Arc<dyn uqa_sql::expr::EngineHook + 'a>,
        retained: &'a uqa_storage::read_control::StorageReadControl,
        invoking: &'a uqa_core::CancellationToken,
    ) -> Option<Self> {
        let format = match format {
            ExplainSerialization::None => return None,
            ExplainSerialization::Text => ResultFormat::Text,
            ExplainSerialization::Binary => ResultFormat::Binary,
        };
        Some(Self {
            format,
            timing,
            engine,
            retained,
            invoking,
            positions: RefCell::new(None),
            metrics: Cell::new(ExplainSerializationMetrics::default()),
        })
    }

    pub(super) fn begin(&self, columns: &[String], schema: &RowSchema) -> Result<(), SQLError> {
        let positions = crate::columnar_batch::result_column_positions_with_control(
            schema,
            columns,
            &self.control(),
        )?;
        self.positions.replace(Some(positions));
        Ok(())
    }

    pub(super) fn consume(
        &self,
        schema: &RowSchema,
        row: &crate::PhysicalRow,
    ) -> Result<(), SQLError> {
        let control = self.control();
        control.check()?;
        let io = Operation::enter(&IOContext::current(), self.retained.memory())?;
        let started = self.timing.then(Instant::now);
        let positions = self.positions.borrow();
        let positions = positions.as_ref().ok_or_else(|| {
            SQLError::Internal("serialization receiver has no row descriptor".into())
        })?;
        let view = schema.view(row);
        let encoded = encode_postgres_row_with_control(
            positions.iter().map(|position| {
                (
                    position.and_then(|position| view.value_at(position)),
                    position
                        .and_then(|position| schema.column_types().get(position))
                        .and_then(Option::as_ref),
                )
            }),
            self.format,
            Some(self.engine.as_ref()),
            &control,
        )?;
        let mut metrics = self.metrics.get();
        metrics.bytes = metrics
            .bytes
            .checked_add(u64::try_from(encoded.len()).map_err(|_| overflow())?)
            .ok_or_else(overflow)?;
        drop(encoded);
        if let Some(started) = started {
            metrics.elapsed = metrics
                .elapsed
                .checked_add(started.elapsed())
                .ok_or_else(measurement_overflow)?;
        }
        metrics.io = metrics
            .io
            .checked_add(
                io.as_ref()
                    .map_or(Ok(IOUsage::default()), Operation::finish)?,
            )
            .ok_or_else(measurement_overflow)?;
        self.metrics.set(metrics);
        Ok(())
    }

    pub(super) fn metrics(&self) -> ExplainSerializationMetrics {
        self.metrics.get()
    }

    fn control(&self) -> ProductionControl<'a> {
        ProductionControl::new(
            self.retained.memory(),
            self.retained.cancellation(),
            self.invoking,
        )
    }
}

fn overflow() -> SQLError {
    SQLError::Routine {
        sqlstate: "54000".into(),
        message: "serialized result volume exceeds u64".into(),
    }
}

fn measurement_overflow() -> SQLError {
    SQLError::Internal("EXPLAIN serialization measurements overflowed".into())
}

#[cfg(test)]
mod tests;
