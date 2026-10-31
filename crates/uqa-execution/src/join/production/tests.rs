//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{Batch, ExecResult, ExpressionEvaluator, NestedLoopJoin};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

struct Input {
    schema: RowSchema,
    closes: Arc<AtomicUsize>,
}
impl PhysicalOperator for Input {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }
    fn open(&mut self) -> ExecResult<()> {
        Ok(())
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        Ok(None)
    }
    fn close(&mut self) -> ExecResult<()> {
        self.closes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}
struct Evaluator;
impl ExpressionEvaluator for Evaluator {
    fn evaluate(&self, _: &ScalarExpr, _: &dyn uqa_sql::expr::RowLookup) -> ExecResult<Value> {
        panic!("construction must not evaluate rows")
    }
}

enum Join {
    Hash(Box<HashJoin<'static>>),
    Nested(Box<NestedLoopJoin<'static>>),
}

fn build(
    hash: bool,
    closes: &Arc<AtomicUsize>,
    keys: [Produced<Vec<ScalarExpr>>; 2],
    control: &ProductionControl<'_>,
) -> Result<Join, SQLError> {
    let input = || {
        Box::new(Input {
            schema: RowSchema::new(vec!["id".into()]),
            closes: closes.clone(),
        })
    };
    if hash {
        let [left_keys, right_keys] = keys;
        HashJoin::with_owned_expressions(
            input(),
            input(),
            JoinKind::Inner,
            left_keys,
            right_keys,
            None,
            Arc::new(Evaluator),
            ResultRow::new(),
            ResultRow::new(),
            1024,
            &[],
            control,
        )
        .map(Box::new)
        .map(Join::Hash)
    } else {
        NestedLoopJoin::with_owned_predicate(
            input(),
            input(),
            JoinKind::Inner,
            None,
            Arc::new(Evaluator),
            ResultRow::new(),
            ResultRow::new(),
            1024,
            control,
        )
        .map(Box::new)
        .map(Join::Nested)
    }
}

fn keys(control: &ProductionControl<'_>) -> [Produced<Vec<ScalarExpr>>; 2] {
    std::array::from_fn(|_| {
        control
            .finish(Vec::new(), control.empty_reservation())
            .unwrap()
    })
}

#[test]
fn join_construction_and_wrapper_failures_close_both_owned_inputs() {
    for hash in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let closes = Arc::new(AtomicUsize::new(0));
        let occupied = memory.reserve(memory.available()).unwrap();
        assert_eq!(
            build(hash, &closes, keys(&control), &control)
                .err()
                .unwrap()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(closes.load(Ordering::Relaxed), 2);
        assert_eq!(memory.used(), occupied.bytes());
        drop(occupied);
        for token in [&original, &invoking] {
            closes.store(0, Ordering::Relaxed);
            let keys = keys(&control);
            token.cancel();
            assert_eq!(
                build(hash, &closes, keys, &control)
                    .err()
                    .unwrap()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(closes.load(Ordering::Relaxed), 2);
            assert_eq!(memory.used(), 0);
            token.reset();
        }
        closes.store(0, Ordering::Relaxed);
        let join = build(hash, &closes, keys(&control), &control).unwrap();
        let occupied = memory.reserve(memory.available()).unwrap();
        let error = match join {
            Join::Hash(join) => (*join).into_boxed(&control).err().unwrap(),
            Join::Nested(join) => (*join).into_boxed(&control).err().unwrap(),
        };
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(closes.load(Ordering::Relaxed), 2);
        assert_eq!(memory.used(), occupied.bytes());
        drop(occupied);
        assert_eq!(memory.used(), 0);
    }
}
