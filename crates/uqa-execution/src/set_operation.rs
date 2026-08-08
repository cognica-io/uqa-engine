//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Byte-bounded physical SQL set operations.

use std::cmp::Ordering;
use std::sync::Arc;

use uqa_core::Value;
use uqa_sql::ast::SetOpKind;
use uqa_sql::expr::RowLookup;
use uqa_sql::ResultRow;

use crate::batch::DEFAULT_BATCH_SIZE;
use crate::{
    Batch, ExecError, ExecResult, ExpressionEvaluator, ExternalSort, PhysicalOperator, RowSchema,
    ScalarExpr, SortKey,
};

struct ColumnEvaluator;

impl ExpressionEvaluator for ColumnEvaluator {
    fn evaluate(&self, expression: &ScalarExpr, row: &dyn RowLookup) -> ExecResult<Value> {
        let ScalarExpr::Column(column) = expression else {
            return Err(ExecError::Other(
                "set-operation sort key must be a column".into(),
            ));
        };
        Ok(row.column(column).cloned().unwrap_or(Value::Null))
    }
}

struct AlignSchema<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    schema: RowSchema,
}

impl<'a> AlignSchema<'a> {
    fn new(child: Box<dyn PhysicalOperator + 'a>, output: Vec<String>) -> ExecResult<Self> {
        let source = child.schema().to_vec();
        if source.len() != output.len() {
            return Err(ExecError::Other(format!(
                "set-operation inputs have different widths: {} and {}",
                output.len(),
                source.len()
            )));
        }
        let mapping = output.into_iter().zip(source).collect::<Vec<_>>();
        let schema = RowSchema::select(child.row_schema(), &mapping);
        Ok(Self { child, schema })
    }
}

impl PhysicalOperator for AlignSchema<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn open(&mut self) -> ExecResult<()> {
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let Some(batch) = self.child.next()? else {
            return Ok(None);
        };
        Ok(Some(Batch::from_physical_rows(
            self.schema.clone(),
            batch.rows,
        )))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}

struct RowGroup {
    key: Vec<Value>,
    row: ResultRow,
    count: usize,
}

struct RowCursor<'a> {
    operator: Box<dyn PhysicalOperator + 'a>,
    batch: std::vec::IntoIter<ResultRow>,
    lookahead: Option<ResultRow>,
    exhausted: bool,
}

impl<'a> RowCursor<'a> {
    fn new(operator: Box<dyn PhysicalOperator + 'a>) -> Self {
        Self {
            operator,
            batch: Vec::new().into_iter(),
            lookahead: None,
            exhausted: false,
        }
    }

    fn open(&mut self) -> ExecResult<()> {
        self.batch = Vec::new().into_iter();
        self.lookahead = None;
        self.exhausted = false;
        self.operator.open()
    }

    fn next_row(&mut self) -> ExecResult<Option<ResultRow>> {
        loop {
            if let Some(row) = self.batch.next() {
                return Ok(Some(row));
            }
            let Some(batch) = self.operator.next()? else {
                self.exhausted = true;
                return Ok(None);
            };
            self.batch = batch.into_result_rows().into_iter();
        }
    }

    fn take_group(&mut self, schema: &[String]) -> ExecResult<Option<RowGroup>> {
        let first = match self.lookahead.take() {
            Some(row) => row,
            None => match self.next_row()? {
                Some(row) => row,
                None => return Ok(None),
            },
        };
        let key = row_key(&first, schema);
        let mut count = 1_usize;
        while let Some(row) = self.next_row()? {
            if row_key(&row, schema) == key {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| ExecError::Other("set-operation group count overflow".into()))?;
            } else {
                self.lookahead = Some(row);
                break;
            }
        }
        Ok(Some(RowGroup {
            key,
            row: first,
            count,
        }))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.batch = Vec::new().into_iter();
        self.lookahead = None;
        self.exhausted = true;
        self.operator.close()
    }
}

fn row_key(row: &ResultRow, schema: &[String]) -> Vec<Value> {
    schema
        .iter()
        .map(|column| row.get(column).cloned().unwrap_or(Value::Null))
        .collect()
}

/// `UNION` / `INTERSECT` / `EXCEPT` physical operator.
///
/// `UNION ALL` streams its children without sorting. Other forms sort both
/// inputs through byte-bounded external runs and merge adjacent multiplicity
/// counts, avoiding whole-input materialisation and quadratic row scans.
pub struct ExternalSetOperation<'a> {
    left: RowCursor<'a>,
    right: RowCursor<'a>,
    kind: SetOpKind,
    all: bool,
    schema: RowSchema,
    left_group: Option<RowGroup>,
    right_group: Option<RowGroup>,
    pending_row: Option<ResultRow>,
    pending_count: usize,
    union_all_left_done: bool,
}

impl<'a> ExternalSetOperation<'a> {
    pub fn new(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: SetOpKind,
        all: bool,
        work_mem_bytes: usize,
    ) -> ExecResult<Self> {
        let output = left.schema().to_vec();
        let left: Box<dyn PhysicalOperator + 'a> =
            Box::new(AlignSchema::new(left, output.clone())?);
        let right: Box<dyn PhysicalOperator + 'a> =
            Box::new(AlignSchema::new(right, output.clone())?);
        let (left, right) = if matches!((kind, all), (SetOpKind::Union, true)) {
            (left, right)
        } else {
            let keys = output
                .iter()
                .map(|column| SortKey {
                    expr: ScalarExpr::Column(column.clone()),
                    descending: false,
                    nulls_first: Some(true),
                })
                .collect::<Vec<_>>();
            let evaluator = Arc::new(ColumnEvaluator);
            // Both merge inputs are live concurrently. Split the configured
            // budget so their encoded in-memory runs cannot each claim it.
            let per_input = (work_mem_bytes / 2).max(1);
            (
                Box::new(ExternalSort::new(
                    left,
                    keys.clone(),
                    evaluator.clone(),
                    None,
                    per_input,
                )) as Box<dyn PhysicalOperator + 'a>,
                Box::new(ExternalSort::new(right, keys, evaluator, None, per_input))
                    as Box<dyn PhysicalOperator + 'a>,
            )
        };
        Ok(Self {
            left: RowCursor::new(left),
            right: RowCursor::new(right),
            kind,
            all,
            schema: RowSchema::new(output),
            left_group: None,
            right_group: None,
            pending_row: None,
            pending_count: 0,
            union_all_left_done: false,
        })
    }

    fn next_union_all(&mut self) -> ExecResult<Option<Batch>> {
        let mut rows = Vec::with_capacity(DEFAULT_BATCH_SIZE);
        while rows.len() < DEFAULT_BATCH_SIZE {
            let next = if self.union_all_left_done {
                self.right.next_row()?
            } else if let Some(row) = self.left.next_row()? {
                Some(row)
            } else {
                self.union_all_left_done = true;
                self.right.next_row()?
            };
            let Some(row) = next else {
                break;
            };
            rows.push(row);
        }
        if rows.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Batch::new(self.schema.clone(), rows)))
        }
    }

    fn load_groups(&mut self) -> ExecResult<()> {
        if self.left_group.is_none() && !self.left.exhausted {
            self.left_group = self.left.take_group(self.schema.columns())?;
        }
        if self.right_group.is_none() && !self.right.exhausted {
            self.right_group = self.right.take_group(self.schema.columns())?;
        }
        Ok(())
    }

    fn take_left_group(&mut self) -> ExecResult<RowGroup> {
        self.left_group
            .take()
            .ok_or_else(|| ExecError::Other("set-operation selected a missing left group".into()))
    }

    fn take_right_group(&mut self) -> ExecResult<RowGroup> {
        self.right_group
            .take()
            .ok_or_else(|| ExecError::Other("set-operation selected a missing right group".into()))
    }

    fn choose_group(&mut self) -> ExecResult<Option<(ResultRow, usize)>> {
        self.load_groups()?;
        let ordering = match (&self.left_group, &self.right_group) {
            (Some(left), Some(right)) => Some(left.key.cmp(&right.key)),
            (Some(_), None) => Some(Ordering::Less),
            (None, Some(_)) => Some(Ordering::Greater),
            (None, None) => None,
        };
        let Some(ordering) = ordering else {
            return Ok(None);
        };

        let selected = match (self.kind, ordering) {
            (SetOpKind::Union, Ordering::Less) => {
                let group = self.take_left_group()?;
                Some((group.row, 1))
            }
            (SetOpKind::Union, Ordering::Greater) => {
                let group = self.take_right_group()?;
                Some((group.row, 1))
            }
            (SetOpKind::Union, Ordering::Equal) => {
                let group = self.take_left_group()?;
                self.right_group = None;
                Some((group.row, 1))
            }
            (SetOpKind::Intersect, Ordering::Less) => {
                self.left_group = None;
                None
            }
            (SetOpKind::Intersect | SetOpKind::Except, Ordering::Greater) => {
                self.right_group = None;
                None
            }
            (SetOpKind::Intersect, Ordering::Equal) => {
                let left = self.take_left_group()?;
                let right = self.take_right_group()?;
                Some((
                    left.row,
                    if self.all {
                        left.count.min(right.count)
                    } else {
                        1
                    },
                ))
            }
            (SetOpKind::Except, Ordering::Less) => {
                let left = self.take_left_group()?;
                Some((left.row, if self.all { left.count } else { 1 }))
            }
            (SetOpKind::Except, Ordering::Equal) => {
                let left = self.take_left_group()?;
                let right = self.take_right_group()?;
                let count = if self.all {
                    left.count.saturating_sub(right.count)
                } else {
                    0
                };
                Some((left.row, count))
            }
        };
        Ok(selected.filter(|(_, count)| *count > 0))
    }
}

impl PhysicalOperator for ExternalSetOperation<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn open(&mut self) -> ExecResult<()> {
        self.left_group = None;
        self.right_group = None;
        self.pending_row = None;
        self.pending_count = 0;
        self.union_all_left_done = false;
        self.left.open()?;
        self.right.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if matches!((self.kind, self.all), (SetOpKind::Union, true)) {
            return self.next_union_all();
        }
        let mut rows = Vec::with_capacity(DEFAULT_BATCH_SIZE);
        while rows.len() < DEFAULT_BATCH_SIZE {
            if self.pending_count > 0 {
                let row = self.pending_row.as_ref().ok_or_else(|| {
                    ExecError::Other(
                        "set-operation has pending multiplicity without a pending row".into(),
                    )
                })?;
                rows.push(row.clone());
                self.pending_count -= 1;
                if self.pending_count == 0 {
                    self.pending_row = None;
                }
                continue;
            }
            match self.choose_group()? {
                Some((row, count)) => {
                    self.pending_row = Some(row);
                    self.pending_count = count;
                }
                None if self.left.exhausted && self.right.exhausted => break,
                None => {}
            }
        }
        if rows.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Batch::new(self.schema.clone(), rows)))
        }
    }

    fn close(&mut self) -> ExecResult<()> {
        self.left_group = None;
        self.right_group = None;
        self.pending_row = None;
        self.pending_count = 0;
        let left = self.left.close();
        let right = self.right.close();
        crate::physical::with_cleanup(left, right, "close right set-operation input")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physical::run_to_rows;
    use crate::scan::TableScan;

    fn row(value: i64) -> ResultRow {
        [("v".into(), Value::Int(value))].into_iter().collect()
    }

    fn execute(kind: SetOpKind, all: bool, left: &[i64], right: &[i64]) -> Vec<i64> {
        let left = TableScan::from_rows(vec!["v".into()], left.iter().copied().map(row).collect());
        let right = TableScan::from_rows(
            vec!["other".into()],
            right
                .iter()
                .copied()
                .map(|value| [("other".into(), Value::Int(value))].into_iter().collect())
                .collect(),
        );
        let mut set =
            ExternalSetOperation::new(Box::new(left), Box::new(right), kind, all, 1).unwrap();
        run_to_rows(&mut set)
            .unwrap()
            .1
            .into_iter()
            .map(|row| match row.get("v") {
                Some(Value::Int(value)) => *value,
                value => panic!("unexpected set value: {value:?}"),
            })
            .collect()
    }

    #[test]
    fn external_set_semantics_include_bag_multiplicity() {
        assert_eq!(
            execute(SetOpKind::Union, false, &[2, 1, 1], &[3, 2]),
            vec![1, 2, 3]
        );
        assert_eq!(
            execute(SetOpKind::Union, true, &[2, 1, 1], &[3, 2]),
            vec![2, 1, 1, 3, 2]
        );
        assert_eq!(
            execute(SetOpKind::Intersect, true, &[1, 1, 1, 2], &[1, 1, 3]),
            vec![1, 1]
        );
        assert_eq!(
            execute(SetOpKind::Except, true, &[1, 1, 1, 2], &[1, 1, 3]),
            vec![1, 2]
        );
        assert_eq!(
            execute(SetOpKind::Except, false, &[1, 1, 2], &[1, 3]),
            vec![2]
        );
    }
}
