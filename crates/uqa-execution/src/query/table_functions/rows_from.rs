//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streaming zip-longest execution for `ROWS FROM` function groups.

use std::collections::VecDeque;

use crate::{
    Batch, ExecError, ExecResult, PhysicalOperator, PhysicalRow, RowSchema, DEFAULT_BATCH_SIZE,
};
use uqa_core::Value;
use uqa_sql::ast::ColumnType;

struct RowsFromChild<'a> {
    operator: Box<dyn PhysicalOperator + 'a>,
    pending: VecDeque<PhysicalRow>,
    exhausted: bool,
}

/// Pull one row from every member for each output position. Shorter members
/// contribute NULLs after exhaustion, matching `PostgreSQL`'s `ROWS FROM`
/// zip-and-pad contract without materialising any complete member result.
pub struct RowsFromOperator<'a> {
    children: Vec<RowsFromChild<'a>>,
    schema: RowSchema,
    ordinality: bool,
    next_ordinality: i64,
    closed: bool,
    exhausted: bool,
}

impl<'a> RowsFromOperator<'a> {
    pub fn new(operators: Vec<Box<dyn PhysicalOperator + 'a>>, ordinality: bool) -> Self {
        debug_assert!(!operators.is_empty());
        let mut schemas = operators
            .iter()
            .map(|operator| operator.row_schema().clone());
        let mut schema = schemas.next().unwrap_or_default();
        for child in schemas {
            schema = RowSchema::join(&schema, &child, std::iter::empty());
        }
        if ordinality {
            let ordinality_schema = RowSchema::with_types(
                vec!["ordinality".into()],
                vec![Some(ColumnType::BigInteger)],
            );
            schema = RowSchema::join(&schema, &ordinality_schema, std::iter::empty());
        }
        Self {
            children: operators
                .into_iter()
                .map(|operator| RowsFromChild {
                    operator,
                    pending: VecDeque::new(),
                    exhausted: false,
                })
                .collect(),
            schema,
            ordinality,
            next_ordinality: 1,
            closed: false,
            exhausted: false,
        }
    }

    fn next_child_row(
        child: &mut RowsFromChild<'a>,
        demanded: bool,
    ) -> ExecResult<Option<PhysicalRow>> {
        loop {
            if let Some(row) = child.pending.pop_front() {
                return Ok(Some(row));
            }
            if child.exhausted {
                return Ok(None);
            }
            match if demanded {
                child
                    .operator
                    .next_with_demand(crate::physical::demand::ONE)?
            } else {
                child.operator.next()?
            } {
                Some(batch) => child.pending.extend(batch.rows),
                None => child.exhausted = true,
            }
        }
    }

    fn close_children(&mut self) -> ExecResult<()> {
        let mut result = Ok(());
        for child in &mut self.children {
            child.pending.clear();
            child.exhausted = true;
            result = crate::physical::with_cleanup(
                result,
                child.operator.close(),
                "close ROWS FROM member",
            );
        }
        result
    }
}

impl PhysicalOperator for RowsFromOperator<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        self.children
            .iter()
            .filter_map(|child| child.operator.estimated_cardinality())
            .max()
    }

    fn backward_scan_support(&self) -> crate::BackwardScanSupport {
        crate::BackwardScanSupport::Materialize
    }

    fn open(&mut self) -> ExecResult<()> {
        self.next_ordinality = 1;
        self.exhausted = false;
        self.closed = false;
        for position in 0..self.children.len() {
            self.children[position].pending.clear();
            self.children[position].exhausted = false;
            if let Err(open_error) = self.children[position].operator.open() {
                return crate::physical::with_cleanup(
                    Err(open_error),
                    self.close(),
                    "close ROWS FROM after open failure",
                );
            }
        }
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.pull(DEFAULT_BATCH_SIZE, false)
    }
    fn next_with_demand(&mut self, rows: std::num::NonZeroUsize) -> ExecResult<Option<Batch>> {
        self.pull(rows.get().min(DEFAULT_BATCH_SIZE), true)
    }

    fn close(&mut self) -> ExecResult<()> {
        self.exhausted = true;
        if std::mem::replace(&mut self.closed, true) {
            Ok(())
        } else {
            self.close_children()
        }
    }
}

impl RowsFromOperator<'_> {
    fn pull(&mut self, rows: usize, demanded: bool) -> ExecResult<Option<Batch>> {
        if self.exhausted {
            return Ok(None);
        }
        let mut output = Vec::with_capacity(rows);
        while output.len() < rows {
            let mut any_member_produced = false;
            let mut row = PhysicalRow::default();
            for child in &mut self.children {
                let member_row = Self::next_child_row(child, demanded)?;
                any_member_produced |= member_row.is_some();
                let member_row = member_row.unwrap_or_else(|| {
                    PhysicalRow::nulls(child.operator.row_schema().physical_width())
                });
                row = PhysicalRow::concat_left_owned(row, &member_row);
            }
            if !any_member_produced {
                self.exhausted = true;
                break;
            }
            if self.ordinality {
                row = row.append_values(vec![Value::Int(self.next_ordinality)]);
                self.next_ordinality = self.next_ordinality.checked_add(1).ok_or_else(|| {
                    ExecError::Other("ROWS FROM ordinality exceeded bigint range".into())
                })?;
            }
            output.push(row);
        }
        if output.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Batch::from_physical_rows(self.schema.clone(), output)))
        }
    }
}
