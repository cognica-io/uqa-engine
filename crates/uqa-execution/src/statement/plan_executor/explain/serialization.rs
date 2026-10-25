//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Measure actual typed output conversion without retaining a second query result.

use crate::{RowSchema, RowSchemaExecution};
use std::{
    cell::{Cell, RefCell},
    sync::Arc,
    time::Instant,
};
use uqa_core::memory::{Produced, ProductionControl};
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
    engine: Arc<dyn crate::query::set_projection::SetFunctionRuntime + 'a>,
    control: ProductionControl<'a>,
    positions: RefCell<Option<Produced<Vec<Option<usize>>>>>,
    metrics: Cell<ExplainSerializationMetrics>,
}

impl<'a> RowSerialization<'a> {
    pub(super) fn new(
        format: ExplainSerialization,
        timing: bool,
        engine: Arc<dyn crate::query::set_projection::SetFunctionRuntime + 'a>,
        control: ProductionControl<'a>,
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
            control,
            positions: RefCell::new(None),
            metrics: Cell::new(ExplainSerializationMetrics::default()),
        })
    }

    pub(super) fn begin(&self, columns: &[String], schema: &RowSchema) -> Result<(), SQLError> {
        let positions = crate::columnar_batch::result_column_positions_with_control(
            schema,
            columns,
            &self.control,
        )?;
        self.positions.replace(Some(positions));
        Ok(())
    }

    pub(super) fn consume(
        &self,
        schema: &RowSchema,
        row: &crate::PhysicalRow,
    ) -> Result<(), SQLError> {
        self.control.check()?;
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
            &self.control,
        )?;
        let mut metrics = self.metrics.get();
        metrics.bytes = metrics
            .bytes
            .checked_add(u64::try_from(encoded.len()).map_err(|_| overflow())?)
            .ok_or_else(overflow)?;
        drop(encoded);
        if let Some(started) = started {
            metrics.elapsed += started.elapsed();
        }
        self.metrics.set(metrics);
        Ok(())
    }

    pub(super) fn metrics(&self) -> ExplainSerializationMetrics {
        self.metrics.get()
    }
}

fn overflow() -> SQLError {
    SQLError::Routine {
        sqlstate: "54000".into(),
        message: "serialized result volume exceeds u64".into(),
    }
}
