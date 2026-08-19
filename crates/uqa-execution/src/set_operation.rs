//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Byte-bounded physical SQL set operations.

use std::cmp::Ordering;
use std::sync::Arc;

use uqa_core::Value;
use uqa_sql::ast::{ColumnType, SetOpKind};
use uqa_sql::expr::RowLookup;

#[cfg(test)]
use uqa_sql::ResultRow;

use crate::batch::DEFAULT_BATCH_SIZE;
use crate::{
    Batch, ExecError, ExecResult, ExpressionEvaluator, ExternalSort, PhysicalOperator, PhysicalRow,
    RowProjectionValue, RowSchema, ScalarExpr, SortKey,
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

fn set_operation_types(left: &RowSchema, right: &RowSchema) -> ExecResult<Vec<Option<ColumnType>>> {
    if left.len() != right.len() {
        return Err(ExecError::Other(format!(
            "set-operation inputs have different widths: {} and {}",
            left.len(),
            right.len()
        )));
    }
    left.column_types()
        .iter()
        .zip(right.column_types())
        .map(|(left, right)| match (left, right) {
            (None, None) => Ok(None),
            (Some(ty), None) | (None, Some(ty)) => Ok(Some(ty.clone())),
            (Some(left), Some(right)) => uqa_execution_common_type(left, right).map(Some),
        })
        .collect()
}

fn uqa_execution_common_type(left: &ColumnType, right: &ColumnType) -> ExecResult<ColumnType> {
    crate::common_type(left, right).map_err(ExecError::from)
}

fn coerce_set_value(
    value: Value,
    source_type: Option<&ColumnType>,
    target_type: &ColumnType,
) -> ExecResult<Value> {
    let cast_target = match target_type {
        ColumnType::Domain { base, .. } => base.as_ref(),
        target => target,
    };
    let source_name = source_type.map(ColumnType::sql_name);
    uqa_sql::expr::cast_value_from(&value, &cast_target.sql_name(), source_name.as_deref())
        .map_err(ExecError::from)
}

struct AlignSchema<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    schema: RowSchema,
    coercions: Vec<Option<ColumnType>>,
}

impl<'a> AlignSchema<'a> {
    fn new(
        child: Box<dyn PhysicalOperator + 'a>,
        output: Vec<String>,
        output_types: &[Option<ColumnType>],
    ) -> ExecResult<Self> {
        let source = child.schema().to_vec();
        if source.len() != output.len() {
            return Err(ExecError::Other(format!(
                "set-operation inputs have different widths: {} and {}",
                output.len(),
                source.len()
            )));
        }
        if output.len() != output_types.len() {
            return Err(ExecError::Other(format!(
                "set-operation output type width {} does not match input width {}",
                output_types.len(),
                output.len()
            )));
        }
        let coercions = child
            .row_schema()
            .column_types()
            .iter()
            .zip(output_types)
            .map(|(source, target)| {
                target
                    .as_ref()
                    .filter(|target| source.as_ref() != Some(*target))
                    .cloned()
            })
            .collect();
        let schema = RowSchema::with_types(output, output_types.to_vec());
        Ok(Self {
            child,
            schema,
            coercions,
        })
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
        let identity_layout = batch.schema.physical_width() == self.schema.physical_width()
            && (0..batch.schema.len())
                .all(|position| batch.schema.slot(position) == Some(position));
        if self.coercions.iter().all(Option::is_none) && identity_layout {
            let rows = batch
                .rows
                .into_iter()
                .map(PhysicalRow::without_lock_origins)
                .collect();
            return Ok(Some(Batch::from_physical_rows(self.schema.clone(), rows)));
        }
        if self.coercions.iter().all(Option::is_none) {
            let slots = (0..batch.schema.len())
                .map(|position| {
                    batch.schema.slot(position).ok_or_else(|| {
                        ExecError::Other(format!(
                            "set-operation input column {position} has no physical slot"
                        ))
                    })
                })
                .collect::<ExecResult<Vec<_>>>()?;
            let rows = batch
                .rows
                .into_iter()
                .map(|row| row.project_slots(&slots).without_lock_origins())
                .collect();
            return Ok(Some(Batch::from_physical_rows(self.schema.clone(), rows)));
        }
        let mut rows = Vec::with_capacity(batch.rows.len());
        for row in batch.rows {
            let view = batch.schema.view(&row);
            let values = self
                .coercions
                .iter()
                .enumerate()
                .map(|(position, target)| match target {
                    Some(target) => coerce_set_value(
                        view.value_at(position).cloned().unwrap_or(Value::Null),
                        batch.schema.column_type(position),
                        target,
                    )
                    .map(RowProjectionValue::Owned),
                    None => Ok(batch.schema.slot(position).map_or(
                        RowProjectionValue::Owned(Value::Null),
                        RowProjectionValue::InputSlot,
                    )),
                })
                .collect::<ExecResult<Vec<_>>>()?;
            rows.push(row.project_with_values(values).without_lock_origins());
        }
        Ok(Some(Batch::from_physical_rows(self.schema.clone(), rows)))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}

struct RowGroup {
    row: PhysicalRow,
    count: usize,
}

struct RowCursor<'a> {
    operator: Box<dyn PhysicalOperator + 'a>,
    batch: std::vec::IntoIter<PhysicalRow>,
    lookahead: Option<PhysicalRow>,
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

    fn next_row(&mut self) -> ExecResult<Option<PhysicalRow>> {
        loop {
            if let Some(row) = self.batch.next() {
                return Ok(Some(row));
            }
            let Some(batch) = self.operator.next()? else {
                self.exhausted = true;
                return Ok(None);
            };
            self.batch = batch.rows.into_iter();
        }
    }

    fn take_group(&mut self, schema: &RowSchema) -> ExecResult<Option<RowGroup>> {
        let first = match self.lookahead.take() {
            Some(row) => row,
            None => match self.next_row()? {
                Some(row) => row,
                None => return Ok(None),
            },
        };
        let mut count = 1_usize;
        while let Some(row) = self.next_row()? {
            if compare_rows(&first, &row, schema) == Ordering::Equal {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| ExecError::Other("set-operation group count overflow".into()))?;
            } else {
                self.lookahead = Some(row);
                break;
            }
        }
        Ok(Some(RowGroup { row: first, count }))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.batch = Vec::new().into_iter();
        self.lookahead = None;
        self.exhausted = true;
        self.operator.close()
    }
}

fn compare_rows(left: &PhysicalRow, right: &PhysicalRow, schema: &RowSchema) -> Ordering {
    let left = schema.view(left);
    let right = schema.view(right);
    let null = Value::Null;
    for position in 0..schema.len() {
        let ordering = left
            .value_at(position)
            .unwrap_or(&null)
            .cmp(right.value_at(position).unwrap_or(&null));
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
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
    pending_row: Option<PhysicalRow>,
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
        let output_types = set_operation_types(left.row_schema(), right.row_schema())?;
        Self::new_with_types(left, right, kind, all, output_types, work_mem_bytes)
    }

    pub fn new_with_types(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: SetOpKind,
        all: bool,
        output_types: Vec<Option<ColumnType>>,
        work_mem_bytes: usize,
    ) -> ExecResult<Self> {
        let output = left.schema().to_vec();
        let left: Box<dyn PhysicalOperator + 'a> =
            Box::new(AlignSchema::new(left, output.clone(), &output_types)?);
        let right: Box<dyn PhysicalOperator + 'a> =
            Box::new(AlignSchema::new(right, output.clone(), &output_types)?);
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
            schema: RowSchema::with_types(output, output_types),
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
            rows.push(row.without_lock_origins());
        }
        if rows.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Batch::from_physical_rows(self.schema.clone(), rows)))
        }
    }

    fn load_groups(&mut self) -> ExecResult<()> {
        if self.left_group.is_none() && !self.left.exhausted {
            self.left_group = self.left.take_group(&self.schema)?;
        }
        if self.right_group.is_none() && !self.right.exhausted {
            self.right_group = self.right.take_group(&self.schema)?;
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

    fn choose_group(&mut self) -> ExecResult<Option<(PhysicalRow, usize)>> {
        self.load_groups()?;
        let ordering = match (&self.left_group, &self.right_group) {
            (Some(left), Some(right)) => Some(compare_rows(&left.row, &right.row, &self.schema)),
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
                    self.pending_row = Some(row.without_lock_origins());
                    self.pending_count = count;
                }
                None if self.left.exhausted && self.right.exhausted => break,
                None => {}
            }
        }
        if rows.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Batch::from_physical_rows(self.schema.clone(), rows)))
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

    #[test]
    fn set_inputs_compact_schema_only_projections_before_alignment() {
        let left = TableScan::from_rows(
            vec!["v".into(), "hidden".into()],
            vec![[
                ("v".into(), Value::Int(1)),
                ("hidden".into(), Value::Int(99)),
            ]
            .into_iter()
            .collect()],
        );
        let left: Box<dyn PhysicalOperator> = Box::new(crate::ColumnSelection::with_positions(
            Box::new(left),
            vec![("v".into(), 0)],
        ));
        let right = TableScan::from_rows(vec!["v".into()], vec![row(2)]);
        let mut set =
            ExternalSetOperation::new(left, Box::new(right), SetOpKind::Union, true, 1).unwrap();
        let rows = run_to_rows(&mut set).unwrap().1;
        assert_eq!(rows, vec![row(1), row(2)]);
    }
}
