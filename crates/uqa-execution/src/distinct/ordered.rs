//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable DISTINCT for keys with fallible SQL comparison. Existing external runs bound the sort; the first row never invokes a key comparison.

use crate::relational::DefaultExpressionEvaluator;
use crate::{
    Batch, ExecError, ExecResult, ExternalSort, PhysicalOperator, RowSchema, ScalarExpr,
    SharedExpressionEvaluator, SortKey,
};
use uqa_core::Value;

pub(crate) fn operator<'a>(
    child: Box<dyn PhysicalOperator + 'a>,
    keys: Vec<ScalarExpr>,
    evaluator: SharedExpressionEvaluator<'a>,
    work_mem: usize,
) -> Box<dyn PhysicalOperator + 'a> {
    let (schema, slots) = child.row_schema().canonical_projection();
    let width = slots.len();
    let key_count = keys.len();
    let relation = uqa_sql::ast::InternalRelationId::allocate();
    let record_schema =
        RowSchema::with_internal_relation_types(relation, vec![None; width + key_count + 1]);
    let child = Box::new(Decorate {
        child,
        schema: record_schema,
        slots,
        keys: crate::scalar::PreparedExpressions::scalars(keys),
        evaluator,
        ordinal: 0,
    });
    let keys = (width..width + key_count)
        .map(|position| sort_key(relation.column(position)))
        .collect::<Vec<_>>();
    let budget = (work_mem / 3).max(1);
    let sorted = Box::new(ExternalSort::new(
        child,
        keys.clone(),
        DefaultExpressionEvaluator::shared(Vec::new()),
        None,
        budget,
    ));
    let unique = Box::new(Adjacent {
        sorted,
        keys,
        width,
        previous: None,
    });
    let stable = ExternalSort::new(
        unique,
        vec![sort_key(relation.column(width + key_count))],
        DefaultExpressionEvaluator::shared(Vec::new()),
        None,
        budget,
    );
    Box::new(OrderedDistinct {
        stable,
        schema,
        slots: (0..width).collect(),
    })
}

fn sort_key(column: uqa_sql::ast::InternalColumnRef) -> SortKey {
    SortKey {
        expr: ScalarExpr::InternalColumn(column),
        descending: false,
        nulls_first: Some(true),
    }
}

struct Decorate<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    schema: RowSchema,
    slots: Vec<usize>,
    keys: crate::scalar::PreparedExpressions<Vec<ScalarExpr>>,
    evaluator: SharedExpressionEvaluator<'a>,
    ordinal: u64,
}

impl PhysicalOperator for Decorate<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }
    fn open(&mut self) -> ExecResult<()> {
        self.ordinal = 0;
        self.child.open()
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let Some(batch) = self.child.next()? else {
            return Ok(None);
        };
        let mut rows = Vec::with_capacity(batch.rows.len());
        for row in batch.rows {
            let mut keys = self
                .keys
                .iter()
                .map(|key| {
                    self.evaluator.evaluate_physical_with_function_states(
                        key,
                        &batch.schema,
                        &row,
                        self.keys.calls(),
                    )
                })
                .collect::<ExecResult<Vec<_>>>()?;
            keys.push(Value::Bytes(self.ordinal.to_be_bytes().to_vec()));
            self.ordinal = self
                .ordinal
                .checked_add(1)
                .ok_or_else(|| ExecError::Other("DISTINCT input sequence overflow".into()))?;
            rows.push(
                row.project_slots(&self.slots)
                    .append_values(keys)
                    .without_lock_origins(),
            );
        }
        Ok(Some(Batch::from_physical_rows(self.schema.clone(), rows)))
    }
    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}

struct Adjacent<'a> {
    sorted: Box<dyn PhysicalOperator + 'a>,
    keys: Vec<SortKey>,
    width: usize,
    previous: Option<Vec<Value>>,
}

impl PhysicalOperator for Adjacent<'_> {
    fn row_schema(&self) -> &RowSchema {
        self.sorted.row_schema()
    }
    fn open(&mut self) -> ExecResult<()> {
        self.previous = None;
        self.sorted.open()
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        while let Some(batch) = self.sorted.next()? {
            let mut rows = Vec::with_capacity(batch.rows.len());
            for row in batch.rows {
                // The private relation enumerates every original physical slot before its keys.
                let values = (self.width..self.width + self.keys.len())
                    .map(|position| row.value(position).unwrap_or(&Value::Null).clone())
                    .collect::<Vec<_>>();
                if let Some(previous) = &self.previous {
                    if crate::relational::compare_sort_key_values(&self.keys, previous, &values)?
                        .is_eq()
                    {
                        continue;
                    }
                }
                self.previous = Some(values);
                rows.push(row);
            }
            if !rows.is_empty() {
                return Ok(Some(Batch::from_physical_rows(batch.schema, rows)));
            }
        }
        Ok(None)
    }
    fn close(&mut self) -> ExecResult<()> {
        self.previous = None;
        self.sorted.close()
    }
}

struct OrderedDistinct<'a> {
    stable: ExternalSort<'a>,
    schema: RowSchema,
    slots: Vec<usize>,
}

impl PhysicalOperator for OrderedDistinct<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }
    fn open(&mut self) -> ExecResult<()> {
        self.stable.open()
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        Ok(self.stable.next()?.map(|batch| {
            Batch::from_physical_rows(
                self.schema.clone(),
                batch
                    .rows
                    .into_iter()
                    .map(|row| row.project_slots(&self.slots))
                    .collect(),
            )
        }))
    }
    fn close(&mut self) -> ExecResult<()> {
        self.stable.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PhysicalRow, TableScan};

    #[test]
    fn ordered_distinct_preserves_first_rows_and_input_order_across_spill() {
        let old = Value::Record(vec![(
            "a".into(),
            Value::Datum(uqa_core::DatumValue::new(
                17,
                0,
                vec![13, b'h', b'e', b'l', b'l', b'o'],
            )),
        )]);
        let fresh = Value::Record(vec![("a".into(), Value::Bytes(b"hello".to_vec()))]);
        let mut hash = crate::CanonicalRowHashSet::new();
        assert!(hash.insert_values(std::slice::from_ref(&old)).unwrap());
        assert!(!hash.insert_values(std::slice::from_ref(&fresh)).unwrap());
        assert!(hash.contains_borrowed(&[&fresh]).unwrap());
        let other = Value::Record(vec![("a".into(), Value::Bytes(b"abc".to_vec()))]);
        for budget in [1, 1024 * 1024] {
            let scan = TableScan::from_physical_rows(
                RowSchema::new(vec!["key".into(), "id".into()]),
                vec![
                    PhysicalRow::from_values(vec![old.clone(), Value::Int(7)]),
                    PhysicalRow::from_values(vec![other.clone(), Value::Int(8)]),
                    PhysicalRow::from_values(vec![fresh.clone(), Value::Int(9)]),
                    PhysicalRow::from_values(vec![Value::Null, Value::Int(10)]),
                    PhysicalRow::from_values(vec![Value::Null, Value::Int(11)]),
                ],
            );
            let mut distinct = operator(
                Box::new(scan),
                vec![ScalarExpr::Position(0)],
                DefaultExpressionEvaluator::shared(Vec::new()),
                budget,
            );
            let batches = crate::physical::run_to_batches(distinct.as_mut()).unwrap();
            let ids = batches
                .into_iter()
                .flat_map(|b| b.rows)
                .map(|row| row.value(1).unwrap().clone())
                .collect::<Vec<_>>();
            assert_eq!(ids, vec![Value::Int(7), Value::Int(8), Value::Int(10)]);
        }
    }

    #[test]
    fn ordered_distinct_reads_a_malformed_key_only_when_comparing_two_rows() {
        let bad = Value::Record(vec![(
            "a".into(),
            Value::Datum(uqa_core::DatumValue::new(
                1700,
                0,
                vec![2, 0, 0, 0, 3, b'x'],
            )),
        )]);
        for budget in [1, 1024 * 1024] {
            for count in 0..=2 {
                let scan = TableScan::from_physical_rows(
                    RowSchema::new(vec!["key".into()]),
                    (0..count)
                        .map(|_| PhysicalRow::from_values(vec![bad.clone()]))
                        .collect(),
                );
                let mut distinct = operator(
                    Box::new(scan),
                    vec![ScalarExpr::Position(0)],
                    DefaultExpressionEvaluator::shared(Vec::new()),
                    budget,
                );
                let result = crate::physical::run_to_batches(distinct.as_mut());
                if count < 2 {
                    assert_eq!(
                        result.unwrap().iter().map(|b| b.rows.len()).sum::<usize>(),
                        count
                    );
                } else {
                    assert!(result
                        .unwrap_err()
                        .to_string()
                        .contains("compressed pglz data is corrupt"));
                }
            }
        }
    }
}
