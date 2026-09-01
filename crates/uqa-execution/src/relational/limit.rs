//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streaming LIMIT/OFFSET, including ordered `FETCH ... WITH TIES`.

use super::{
    compare_sort_key_values, BackwardScanSupport, Batch, ExecError, ExecResult, PhysicalOperator,
    PhysicalScanDirection, RowSchema, ScalarExpr, SharedExpressionEvaluator, SortKey, Value,
};
use crate::PhysicalRow;

struct WithTies<'a> {
    keys: Vec<SortKey>,
    evaluator: SharedExpressionEvaluator<'a>,
    boundary: Option<Vec<Value>>,
    finished: bool,
}

#[derive(Clone, Copy)]
enum DirectionalLimitState {
    Initial,
    Rescan,
    Empty,
    InWindow,
    SubplanEof,
    WindowEnd,
    WindowEndTies,
    WindowStart,
}

pub struct Limit<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    offset: u64,
    limit: Option<u64>,
    skipped: u64,
    emitted: u64,
    schema: RowSchema,
    with_ties: Option<WithTies<'a>>,
    directional_state: DirectionalLimitState,
    directional_position: u64,
    directional_current: Option<PhysicalRow>,
}

impl<'a> Limit<'a> {
    pub fn new(child: Box<dyn PhysicalOperator + 'a>, offset: u64, limit: Option<u64>) -> Self {
        let schema = child.row_schema().clone();
        Self {
            child,
            offset,
            limit,
            skipped: 0,
            emitted: 0,
            schema,
            with_ties: None,
            directional_state: DirectionalLimitState::Initial,
            directional_position: 0,
            directional_current: None,
        }
    }

    /// Build an ordered `FETCH ... WITH TIES` boundary. The caller must pass the complete effective `ORDER BY` key list and a non-null row count.
    pub fn with_ties(
        child: Box<dyn PhysicalOperator + 'a>,
        offset: u64,
        limit: u64,
        mut keys: Vec<SortKey>,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
        let schema = child.row_schema().clone();
        for key in &mut keys {
            let expression = std::mem::replace(&mut key.expr, ScalarExpr::Literal(Value::Null));
            key.expr = evaluator.bind_type_introspection(expression, &schema);
        }
        Self {
            child,
            offset,
            limit: Some(limit),
            skipped: 0,
            emitted: 0,
            schema,
            with_ties: Some(WithTies {
                keys,
                evaluator,
                boundary: None,
                finished: false,
            }),
            directional_state: DirectionalLimitState::Initial,
            directional_position: 0,
            directional_current: None,
        }
    }

    fn reset_directional_state(&mut self) {
        self.directional_state = DirectionalLimitState::Rescan;
        self.directional_position = 0;
        self.directional_current = None;
        if let Some(with_ties) = self.with_ties.as_mut() {
            with_ties.boundary = None;
            with_ties.finished = false;
        }
    }

    fn child_row(&mut self, direction: PhysicalScanDirection) -> ExecResult<Option<PhysicalRow>> {
        let Some(batch) = self.child.next_direction(direction)? else {
            return Ok(None);
        };
        if batch.schema != self.schema {
            return Err(ExecError::Other(format!(
                "directional LIMIT input schema mismatch: expected {:?}, got {:?}",
                self.schema, batch.schema
            )));
        }
        let mut rows = batch.rows.into_iter();
        let row = rows.next();
        if rows.next().is_some() {
            return Err(ExecError::Other(
                "directional LIMIT input returned more than one row".into(),
            ));
        }
        Ok(row)
    }

    fn capture_tie_boundary(&mut self, row: &PhysicalRow) -> ExecResult<()> {
        let Some(with_ties) = self.with_ties.as_mut() else {
            return Ok(());
        };
        with_ties.boundary = Some(
            with_ties
                .keys
                .iter()
                .map(|key| {
                    with_ties
                        .evaluator
                        .evaluate_physical(&key.expr, &self.schema, row)
                })
                .collect::<ExecResult<Vec<_>>>()?,
        );
        Ok(())
    }

    fn tie_matches(&self, row: &PhysicalRow) -> ExecResult<bool> {
        let with_ties = self
            .with_ties
            .as_ref()
            .ok_or_else(|| ExecError::Other("LIMIT tie state is absent".into()))?;
        let values = with_ties
            .keys
            .iter()
            .map(|key| {
                with_ties
                    .evaluator
                    .evaluate_physical(&key.expr, &self.schema, row)
            })
            .collect::<ExecResult<Vec<_>>>()?;
        let boundary = with_ties
            .boundary
            .as_ref()
            .ok_or_else(|| ExecError::Other("LIMIT tie boundary is absent".into()))?;
        Ok(
            compare_sort_key_values(&with_ties.keys, boundary, &values)
                == std::cmp::Ordering::Equal,
        )
    }

    fn directional_batch(&self, row: PhysicalRow) -> Batch {
        Batch::from_physical_rows(self.schema.clone(), vec![row])
    }

    #[expect(
        clippy::too_many_lines,
        reason = "mirrors PostgreSQL's directional LIMIT state machine"
    )]
    fn next_directional(&mut self, direction: PhysicalScanDirection) -> ExecResult<Option<Batch>> {
        if matches!(self.directional_state, DirectionalLimitState::Initial) {
            self.reset_directional_state();
        }
        loop {
            match self.directional_state {
                DirectionalLimitState::Initial => unreachable!(),
                DirectionalLimitState::Rescan => {
                    if direction == PhysicalScanDirection::Backward {
                        return Ok(None);
                    }
                    if self.limit == Some(0) {
                        self.directional_state = DirectionalLimitState::Empty;
                        return Ok(None);
                    }
                    loop {
                        let Some(row) = self.child_row(PhysicalScanDirection::Forward)? else {
                            self.directional_state = DirectionalLimitState::Empty;
                            return Ok(None);
                        };
                        if self.with_ties.is_some()
                            && self.limit.is_some_and(|limit| {
                                self.directional_position.saturating_sub(self.offset)
                                    == limit.saturating_sub(1)
                            })
                        {
                            self.capture_tie_boundary(&row)?;
                        }
                        self.directional_current = Some(row.clone());
                        self.directional_position = self
                            .directional_position
                            .checked_add(1)
                            .ok_or_else(|| ExecError::Other("LIMIT position overflow".into()))?;
                        if self.directional_position > self.offset {
                            self.directional_state = DirectionalLimitState::InWindow;
                            return Ok(Some(self.directional_batch(row)));
                        }
                    }
                }
                DirectionalLimitState::Empty => return Ok(None),
                DirectionalLimitState::InWindow => {
                    if direction == PhysicalScanDirection::Forward {
                        if self.limit.is_some_and(|limit| {
                            self.directional_position.saturating_sub(self.offset) >= limit
                        }) {
                            if self.with_ties.is_none() {
                                self.directional_state = DirectionalLimitState::WindowEnd;
                                return Ok(None);
                            }
                            self.directional_state = DirectionalLimitState::WindowEndTies;
                            continue;
                        }
                        let Some(row) = self.child_row(PhysicalScanDirection::Forward)? else {
                            self.directional_state = DirectionalLimitState::SubplanEof;
                            return Ok(None);
                        };
                        if self.with_ties.is_some()
                            && self.limit.is_some_and(|limit| {
                                self.directional_position.saturating_sub(self.offset)
                                    == limit.saturating_sub(1)
                            })
                        {
                            self.capture_tie_boundary(&row)?;
                        }
                        self.directional_current = Some(row.clone());
                        self.directional_position = self
                            .directional_position
                            .checked_add(1)
                            .ok_or_else(|| ExecError::Other("LIMIT position overflow".into()))?;
                        return Ok(Some(self.directional_batch(row)));
                    }
                    if self.directional_position <= self.offset.saturating_add(1) {
                        self.directional_state = DirectionalLimitState::WindowStart;
                        return Ok(None);
                    }
                    let row = self
                        .child_row(PhysicalScanDirection::Backward)?
                        .ok_or_else(|| {
                            ExecError::Other("LIMIT input failed to scan backwards".into())
                        })?;
                    self.directional_current = Some(row.clone());
                    self.directional_position -= 1;
                    return Ok(Some(self.directional_batch(row)));
                }
                DirectionalLimitState::WindowEndTies => {
                    if direction == PhysicalScanDirection::Forward {
                        let Some(row) = self.child_row(PhysicalScanDirection::Forward)? else {
                            self.directional_state = DirectionalLimitState::SubplanEof;
                            return Ok(None);
                        };
                        if self.tie_matches(&row)? {
                            self.directional_current = Some(row.clone());
                            self.directional_position =
                                self.directional_position.checked_add(1).ok_or_else(|| {
                                    ExecError::Other("LIMIT position overflow".into())
                                })?;
                            return Ok(Some(self.directional_batch(row)));
                        }
                        self.directional_state = DirectionalLimitState::WindowEnd;
                        return Ok(None);
                    }
                    if self.directional_position <= self.offset.saturating_add(1) {
                        self.directional_state = DirectionalLimitState::WindowStart;
                        return Ok(None);
                    }
                    let row = self
                        .child_row(PhysicalScanDirection::Backward)?
                        .ok_or_else(|| {
                            ExecError::Other("LIMIT input failed to scan backwards".into())
                        })?;
                    self.directional_current = Some(row.clone());
                    self.directional_position -= 1;
                    self.directional_state = DirectionalLimitState::InWindow;
                    return Ok(Some(self.directional_batch(row)));
                }
                DirectionalLimitState::SubplanEof => {
                    if direction == PhysicalScanDirection::Forward {
                        return Ok(None);
                    }
                    let row = self
                        .child_row(PhysicalScanDirection::Backward)?
                        .ok_or_else(|| {
                            ExecError::Other("LIMIT input failed to leave end position".into())
                        })?;
                    self.directional_current = Some(row.clone());
                    self.directional_state = DirectionalLimitState::InWindow;
                    return Ok(Some(self.directional_batch(row)));
                }
                DirectionalLimitState::WindowEnd => {
                    if direction == PhysicalScanDirection::Forward {
                        return Ok(None);
                    }
                    let row = if self.with_ties.is_some() {
                        self.child_row(PhysicalScanDirection::Backward)?
                            .ok_or_else(|| {
                                ExecError::Other("LIMIT input failed to leave tie boundary".into())
                            })?
                    } else {
                        self.directional_current.clone().ok_or_else(|| {
                            ExecError::Other("LIMIT window has no current row".into())
                        })?
                    };
                    self.directional_current = Some(row.clone());
                    self.directional_state = DirectionalLimitState::InWindow;
                    return Ok(Some(self.directional_batch(row)));
                }
                DirectionalLimitState::WindowStart => {
                    if direction == PhysicalScanDirection::Backward {
                        return Ok(None);
                    }
                    let row = self.directional_current.clone().ok_or_else(|| {
                        ExecError::Other("LIMIT window has no current row".into())
                    })?;
                    self.directional_state = DirectionalLimitState::InWindow;
                    return Ok(Some(self.directional_batch(row)));
                }
            }
        }
    }
}

impl PhysicalOperator for Limit<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn output_ordering(&self) -> &[crate::PhysicalOrder] {
        self.child.output_ordering()
    }

    fn backward_scan_support(&self) -> BackwardScanSupport {
        if self.child.backward_scan_support() == BackwardScanSupport::Native {
            BackwardScanSupport::Native
        } else {
            BackwardScanSupport::Unsupported
        }
    }

    fn open(&mut self) -> ExecResult<()> {
        self.skipped = 0;
        self.emitted = 0;
        if let Some(with_ties) = self.with_ties.as_mut() {
            with_ties.boundary = None;
            with_ties.finished = false;
        }
        self.directional_state = DirectionalLimitState::Initial;
        self.directional_position = 0;
        self.directional_current = None;
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if self.limit == Some(0)
            || self
                .with_ties
                .as_ref()
                .is_some_and(|with_ties| with_ties.finished)
            || self.with_ties.is_none() && self.limit.is_some_and(|limit| self.emitted >= limit)
        {
            return Ok(None);
        }
        loop {
            let Some(batch) = self.child.next()? else {
                return Ok(None);
            };
            let mut buf = Vec::new();
            for row in batch.rows {
                if self.skipped < self.offset {
                    self.skipped += 1;
                    continue;
                }
                if let Some(lim) = self.limit {
                    if self.emitted >= lim {
                        let Some(with_ties) = self.with_ties.as_mut() else {
                            return if buf.is_empty() {
                                Ok(None)
                            } else {
                                Ok(Some(Batch::from_physical_rows(self.schema.clone(), buf)))
                            };
                        };
                        let values = with_ties
                            .keys
                            .iter()
                            .map(|key| {
                                with_ties
                                    .evaluator
                                    .evaluate_physical(&key.expr, &self.schema, &row)
                            })
                            .collect::<ExecResult<Vec<_>>>()?;
                        let boundary = with_ties.boundary.as_ref().ok_or_else(|| {
                            crate::ExecError::Other(
                                "WITH TIES boundary was not captured".to_string(),
                            )
                        })?;
                        if compare_sort_key_values(&with_ties.keys, boundary, &values)
                            != std::cmp::Ordering::Equal
                        {
                            with_ties.finished = true;
                            return if buf.is_empty() {
                                Ok(None)
                            } else {
                                Ok(Some(Batch::from_physical_rows(self.schema.clone(), buf)))
                            };
                        }
                    } else if self.with_ties.is_some() && self.emitted + 1 == lim {
                        let with_ties = self.with_ties.as_mut().expect("checked above");
                        with_ties.boundary = Some(
                            with_ties
                                .keys
                                .iter()
                                .map(|key| {
                                    with_ties.evaluator.evaluate_physical(
                                        &key.expr,
                                        &self.schema,
                                        &row,
                                    )
                                })
                                .collect::<ExecResult<Vec<_>>>()?,
                        );
                    }
                }
                buf.push(row);
                self.emitted += 1;
            }
            if !buf.is_empty() {
                return Ok(Some(Batch::from_physical_rows(self.schema.clone(), buf)));
            }
        }
    }

    fn next_direction(&mut self, direction: PhysicalScanDirection) -> ExecResult<Option<Batch>> {
        self.next_directional(direction)
    }

    fn rewind(&mut self) -> ExecResult<()> {
        self.reset_directional_state();
        self.child.rewind()
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}
