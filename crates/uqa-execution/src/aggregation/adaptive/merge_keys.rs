//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Equality-only aggregate keys merge by exact SQL hash encoding without acquiring an ordering operator.

use crate::{
    Batch, ExecResult, ExternalSort, PhysicalOperator, PhysicalRow, RowSchema,
    SharedExpressionEvaluator, SortKey, SpillBuffer, SpillScan,
};
use uqa_core::Value;
use uqa_sql::{ast::InternalRelationId, ScalarExpr};

pub(super) fn sorted_partials<'a>(
    partials: SpillBuffer,
    relation: InternalRelationId,
    group_count: usize,
    order_group_values: bool,
    evaluator: SharedExpressionEvaluator<'a>,
    budget: usize,
) -> ExternalSort<'a> {
    let schema = super::super::partial_state::partial_schema(relation, group_count);
    let mut scan: Box<dyn PhysicalOperator + 'a> = Box::new(SpillScan::new(schema, partials));
    let positions = if order_group_values {
        0..group_count
    } else {
        scan = Box::new(KeyedPartials::new(
            scan,
            relation,
            group_count,
            std::sync::Arc::clone(&evaluator),
        ));
        group_count + 1..group_count + 2
    };
    let keys = positions
        .map(|index| SortKey {
            expr: ScalarExpr::InternalColumn(relation.column(index)),
            descending: false,
            nulls_first: None,
        })
        .collect();
    ExternalSort::new(scan, keys, evaluator, None, budget)
}

struct KeyedPartials<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    schema: RowSchema,
    group_count: usize,
    evaluator: SharedExpressionEvaluator<'a>,
    pending: std::vec::IntoIter<PhysicalRow>,
}

impl<'a> KeyedPartials<'a> {
    fn new(
        child: Box<dyn PhysicalOperator + 'a>,
        relation: uqa_sql::ast::InternalRelationId,
        group_count: usize,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
        let mut types = vec![None; group_count + 1];
        types.push(Some(uqa_sql::ColumnType::Bytea));
        Self {
            child,
            schema: RowSchema::with_internal_relation_types(relation, types),
            group_count,
            evaluator,
            pending: Vec::new().into_iter(),
        }
    }
}

impl PhysicalOperator for KeyedPartials<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn open(&mut self) -> ExecResult<()> {
        self.pending = Vec::new().into_iter();
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let row = loop {
            if let Some(row) = self.pending.next() {
                break row;
            }
            let Some(batch) = self.child.next()? else {
                return Ok(None);
            };
            self.pending = batch.rows.into_iter();
        };
        // Admit each derived key into the external sort before producing another; do not retain a second key batch outside its allowance.
        let key = crate::distinct::canonical_sql_key(
            (0..self.group_count).map(|position| row.value(position)),
            self.evaluator.enum_labels(),
        )?;
        Ok(Some(Batch::from_physical_rows(
            self.schema.clone(),
            vec![row.append_values(vec![Value::Bytes(key)])],
        )))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.pending = Vec::new().into_iter();
        self.child.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        external_sort::ExternalSort, physical::run_to_batches, scan::TableScan, PhysicalRow,
        SortKey,
    };
    use uqa_sql::ScalarExpr;

    #[test]
    fn spill_merge_keeps_equal_physical_and_admitted_keys_adjacent_without_ordering() {
        let relation = uqa_sql::ast::InternalRelationId::allocate();
        let schema = super::super::super::partial_state::partial_schema(relation, 1);
        let input = [2_u32, 1, 3, 1, 2, 3]
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                let field = if index < 3 {
                    Value::Datum(uqa_core::DatumValue::new(
                        28,
                        0,
                        value.to_le_bytes().to_vec(),
                    ))
                } else {
                    Value::Int(i64::from(value))
                };
                PhysicalRow::from_values(vec![
                    Value::Record(vec![("n".into(), field)].into()),
                    Value::List(vec![Value::Int(index as i64)]),
                ])
            })
            .collect();
        let evaluator = crate::relational::DefaultExpressionEvaluator::shared(Vec::new());
        let scan = TableScan::from_physical_rows(schema, input);
        let keys = KeyedPartials::new(
            Box::new(scan),
            relation,
            1,
            std::sync::Arc::clone(&evaluator),
        );
        let mut sorted = ExternalSort::new(
            Box::new(keys),
            vec![SortKey {
                expr: ScalarExpr::InternalColumn(relation.column(2)),
                descending: false,
                nulls_first: None,
            }],
            evaluator,
            None,
            1,
        );
        let rows = run_to_batches(&mut sorted)
            .unwrap()
            .into_iter()
            .flat_map(|batch| batch.rows)
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 6);
        for pair in rows.chunks_exact(2) {
            assert_eq!(pair[0].value(2), pair[1].value(2));
            assert!(uqa_sql::expr::equal_typed_values_with_enum_catalog(
                pair[0].value(0).unwrap(),
                pair[1].value(0).unwrap(),
                &uqa_core::memory::ProductionControl::uncontrolled(),
                None
            )
            .unwrap());
        }
        assert_ne!(rows[0].value(2), rows[2].value(2));
        assert_ne!(rows[2].value(2), rows[4].value(2));
        let mut states = rows
            .iter()
            .map(|row| {
                let Some(Value::List(values)) = row.value(1) else {
                    panic!("partial state")
                };
                let [Value::Int(index)] = values.as_slice() else {
                    panic!("partial identity")
                };
                *index
            })
            .collect::<Vec<_>>();
        states.sort_unstable();
        assert_eq!(states, (0..6).collect::<Vec<_>>());
    }
}
