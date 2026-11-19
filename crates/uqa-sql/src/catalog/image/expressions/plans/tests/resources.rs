//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{CommandPlan, CtePlanBody, RelationalPlan, SourcePlan};

#[test]
fn query_image_preparation_and_borrowed_payload_do_not_copy_the_plan() {
    let mut value = query("SELECT 'payload' AS result");
    let RelationalPlan::QueryBlock(block) = &mut value.root else {
        panic!("block")
    };
    block.projections[0].alias = Some("q".repeat(1 << 20));
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let mut reader = None;
    let preparation =
        allocation_counter::measure(|| reader = Some(value.image_reader(&memory, &token).unwrap()));
    assert_eq!(preparation.count_total, 0);
    assert_eq!(memory.used(), 0);
    let mut reader = reader.unwrap();
    let mut buffer = vec![0; 32768];
    let count = reader.read(&mut buffer).unwrap();
    assert_eq!(count, 16384);
    let scratch = memory.used();
    assert!(scratch > 0 && scratch < 16384);
    let allocation = allocation_counter::measure(|| {
        assert_eq!(reader.read(&mut buffer).unwrap(), 16384);
    });
    assert_eq!(allocation.count_total, 0);
    assert_eq!(memory.used(), scratch);
    while reader.read(&mut buffer).unwrap() != 0 {}
    assert_eq!(memory.used(), 0);
}

#[test]
fn query_image_admission_and_cancellation_retire_the_active_reader() {
    let value =
        query("SELECT sum(a) OVER (PARTITION BY b ORDER BY a) FROM t LEFT JOIN u USING (a)");
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    let expected = collect(value.image_reader(&memory, &token).unwrap(), 37);
    let peak = memory.peak();
    for limit in [0, 1, peak - 1] {
        let denied = MemoryBudget::new(limit);
        let mut reader = value.image_reader(&denied, &token).unwrap();
        let mut buffer = [0; 37];
        let error = loop {
            match reader.read(&mut buffer) {
                Ok(0) => panic!("insufficient reader admission succeeded"),
                Ok(_) => {}
                Err(error) => break error,
            }
        };
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(denied.used(), 0);
        assert!(reader.read(&mut buffer).is_err());
    }
    let exact = MemoryBudget::new(peak);
    assert_eq!(
        collect(value.image_reader(&exact, &token).unwrap(), 37),
        expected
    );
    assert_eq!(exact.used(), 0);
    let mut reader = value.image_reader(&memory, &token).unwrap();
    assert_eq!(reader.read(&mut [0; 19]).unwrap(), 19);
    token.cancel();
    assert!(value.image_reader(&memory, &token).is_err());
    assert!(reader.read(&mut [0; 19]).is_err());
    assert!(reader.read(&mut [0; 19]).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn query_image_rejects_runtime_sources_and_non_cte_commands_terminally() {
    let mut invalid_command = query("WITH q AS (SELECT 1) SELECT * FROM q");
    invalid_command.ctes[0].body = CtePlanBody::Command(Box::new(CommandPlan::Empty));
    let mut invalid_source = query("SELECT 1");
    let RelationalPlan::QueryBlock(block) = &mut invalid_source.root else {
        panic!("block")
    };
    block.from = Some(SourcePlan::RetainedRows(
        crate::ast::RetainedRowSource::new(
            crate::ast::InternalRelationId::from_raw(1),
            crate::RowSchema::default(),
            1,
        )
        .unwrap(),
    ));
    for value in [invalid_command, invalid_source] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut reader = value.image_reader(&memory, &token).unwrap();
        let mut bytes = [0; 37];
        loop {
            match reader.read(&mut bytes) {
                Ok(0) => panic!("runtime source was accepted"),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert_eq!(memory.used(), 0);
        assert!(reader.read(&mut bytes).is_err());
    }
}

#[test]
fn query_image_traversal_keeps_constant_native_stack_through_deep_query_arenas() {
    let depth = 10000;
    let mut value = query("VALUES (1)");
    for _ in 0..depth {
        value = QueryPlan {
            analyzed_output: None,
            analyzed_source: None,
            analyzed_grouping: None,
            relations_bound: false,
            relation_privilege_subject: None,
            ctes: Vec::new(),
            root: RelationalPlan::Values {
                rows: Vec::new(),
                subqueries: vec![value],
            },
        };
    }
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(move || {
            let memory = MemoryBudget::new(64 << 20);
            let token = CancellationToken::new();
            let reader = value.image_reader(&memory, &token).unwrap();
            assert_eq!(memory.used(), 0);
            let output = collect(reader, 127);
            assert!(output.len() > depth * 20);
            assert_eq!(memory.used(), 0);
            // Retire the synthetic source chain explicitly. Native plan input and its retirement are separate from this borrowed producer.
            loop {
                let RelationalPlan::Values { mut subqueries, .. } = value.root else {
                    panic!("values")
                };
                let Some(child) = subqueries.pop() else { break };
                value = child;
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[derive(Debug)]
struct Unavailable(bool);
impl crate::ir::ConstantValueSource for Unavailable {
    fn read(&self, _: u64) -> Result<uqa_core::Value, SQLError> {
        panic!("query images must not materialize external constants")
    }
    fn resident_value(&self, _: u64) -> Option<&uqa_core::Value> {
        assert!(!self.0, "injected query constant unwind");
        None
    }
}

#[test]
fn query_image_constant_failure_and_unwind_retire_every_continuation() {
    use crate::ir::{RetainedConstantValue, RetainedScalarLiteral};
    for unwind in [false, true] {
        let mut value = query("SELECT 1");
        let RelationalPlan::QueryBlock(block) = &mut value.root else {
            panic!("block")
        };
        block.projections[0].expr = crate::ScalarExpr::RetainedLiteral(RetainedScalarLiteral::new(
            RetainedConstantValue::new(std::sync::Arc::new(Unavailable(unwind)), 0),
            None,
            None,
            None,
        ));
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut reader = value.image_reader(&memory, &token).unwrap();
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader.read(&mut [0; 512])));
        if unwind {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_err());
        }
        assert_eq!(memory.used(), 0);
        assert!(reader.read(&mut [0; 512]).is_err());
    }
}
