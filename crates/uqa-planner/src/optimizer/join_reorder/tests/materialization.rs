//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

pub(super) struct FunctionEstimates;

impl SourceStatistics for FunctionEstimates {
    fn relation_statistics(&self, _: &str) -> Option<crate::RelationStats> {
        None
    }

    fn source_access_estimate(&self, source: &SourcePlan) -> Option<crate::LocalAccessEstimate> {
        let index = source
            .visible_qualifier()?
            .strip_prefix('s')?
            .parse::<usize>()
            .ok()?;
        Some(crate::LocalAccessEstimate {
            output_rows: (65 - index) as f64,
            cost: 1.0,
            paradigm: crate::AccessParadigm::Relational,
        })
    }
}

pub(super) fn fixture(count: usize, payload_bytes: usize) -> SourcePlan {
    let inputs = (0..count)
        .map(|index| format!("regexp_split_to_table('x', ',') AS s{index}(v)"))
        .collect::<Vec<_>>()
        .join(", ");
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(
        uqa_sql::compile(&format!("SELECT * FROM {inputs}"))
            .unwrap()
            .remove(0),
    ) else {
        panic!("query");
    };
    let RelationalPlan::QueryBlock(block) = query.root else {
        panic!("query block");
    };
    let mut source = block.from.unwrap();
    let mut first = &mut source;
    while let SourcePlan::Join { left, .. } = first {
        first = left;
    }
    let SourcePlan::Function { args, .. } = first else {
        panic!("function leaf");
    };
    args[0] = ScalarExpr::Literal(Value::Str("x".repeat(payload_bytes)));
    source
}

#[derive(Debug, PartialEq, Eq)]
struct Allocations {
    boxes: BTreeSet<usize>,
    payloads: BTreeMap<String, usize>,
}

fn allocations(source: &SourcePlan) -> Allocations {
    fn collect(source: &SourcePlan, observed: &mut Allocations) {
        match source {
            SourcePlan::Join { left, right, .. } => {
                observed
                    .boxes
                    .insert(std::ptr::from_ref(left.as_ref()) as usize);
                observed
                    .boxes
                    .insert(std::ptr::from_ref(right.as_ref()) as usize);
                collect(left, observed);
                collect(right, observed);
            }
            SourcePlan::Function { args, alias, .. } => {
                let ScalarExpr::Literal(Value::Str(payload)) = &args[0] else {
                    panic!("text argument");
                };
                observed
                    .payloads
                    .insert(alias.clone().unwrap(), payload.as_ptr() as usize);
            }
            _ => panic!("unexpected source"),
        }
    }
    let mut observed = Allocations {
        boxes: BTreeSet::new(),
        payloads: BTreeMap::new(),
    };
    collect(source, &mut observed);
    observed
}

#[test]
fn selected_sql_join_reuses_source_payloads_and_node_allocations() {
    for count in [2, 8, 64] {
        let mut source = fixture(count, 4 * 1024 * 1024);
        let before = allocations(&source);
        let budget = MemoryBudget::new(16 * 1024 * 1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut selected = None;
        let measured = allocation_counter::measure(|| {
            selected = reordered_inner_join_source(&mut source, &[], &FunctionEstimates, &control)
                .unwrap();
        });
        let selected = selected.unwrap();
        assert!(
            measured.bytes_total < 512 * 1024,
            "count={count}: {measured:?}"
        );
        assert_eq!(allocations(&selected), before, "count={count}");
        assert_eq!(budget.used(), 0);
        assert_eq!(measured.bytes_current, 0, "count={count}: {measured:?}");
        let mut order = Vec::new();
        function_order(&selected, &mut order);
        assert_eq!(
            order,
            (0..count)
                .rev()
                .map(|index| format!("s{index}"))
                .collect::<Vec<_>>()
        );
    }
}

fn function_order<'a>(source: &'a SourcePlan, output: &mut Vec<&'a str>) {
    if let SourcePlan::Join { left, right, .. } = source {
        function_order(left, output);
        function_order(right, output);
    } else {
        output.push(source.visible_qualifier().unwrap());
    }
}

#[test]
fn rejected_sql_join_selection_leaves_source_ownership_unchanged() {
    for mode in 0..4 {
        let mut source = fixture(8, 32);
        let before = allocations(&source);
        let contents = serde_json::to_value(&source).unwrap();
        let budget = MemoryBudget::new(if mode == 0 { 0 } else { 1024 * 1024 });
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if mode == 1 {
            original.cancel();
        } else if mode == 2 {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        if mode == 3 {
            assert!(
                reordered_inner_join_source(&mut source, &[], &|_: &str| None, &control)
                    .unwrap()
                    .is_none()
            );
        } else {
            let error = reordered_inner_join_source(&mut source, &[], &FunctionEstimates, &control)
                .unwrap_err();
            let crate::optimizer::OptimizerError::Expression(error) = error else {
                panic!("typed admission error");
            };
            assert_eq!(
                error.sqlstate(),
                Some(if mode == 0 { "53200" } else { "57014" })
            );
        }
        assert_eq!(allocations(&source), before);
        assert_eq!(serde_json::to_value(&source).unwrap(), contents);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn selected_sql_join_moves_table_execution_plans_without_copying() {
    let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(
        uqa_sql::compile("SELECT * FROM a CROSS JOIN b")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query");
    };
    let UnifiedPlan::Query(mut execution) = UnifiedPlan::lower(
        uqa_sql::compile("SELECT 'payload' AS value")
            .unwrap()
            .remove(0),
    ) else {
        panic!("execution query");
    };
    let payload = "x".repeat(4 * 1024 * 1024);
    let payload_pointer = payload.as_ptr();
    let RelationalPlan::QueryBlock(block) = &mut execution.root else {
        panic!("block");
    };
    block.projections[0].expr = ScalarExpr::Literal(Value::Str(payload));
    let execution_pointer = std::ptr::from_ref(execution.as_ref());
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block");
    };
    let source = block.from.as_mut().unwrap();
    let SourcePlan::Join { left, .. } = source else {
        panic!("join");
    };
    let SourcePlan::Table {
        executable_query, ..
    } = left.as_mut()
    else {
        panic!("table");
    };
    *executable_query = Some(execution);
    let mut selected = None;
    let measured = allocation_counter::measure(|| {
        selected = reordered_inner_join_source(
            source,
            &[],
            &|_: &str| None,
            &ProductionControl::uncontrolled(),
        )
        .unwrap();
    });
    assert!(measured.bytes_total < 16 * 1024, "{measured:?}");
    assert_eq!(measured.bytes_current, 0, "{measured:?}");
    let selected = selected.unwrap();
    let SourcePlan::Join { left, .. } = &*selected else {
        panic!("join");
    };
    let SourcePlan::Table {
        name,
        executable_query: Some(execution),
        ..
    } = left.as_ref()
    else {
        panic!("table execution");
    };
    assert_eq!(name, "a");
    assert_eq!(std::ptr::from_ref(execution.as_ref()), execution_pointer);
    let RelationalPlan::QueryBlock(block) = &execution.root else {
        panic!("execution block");
    };
    let ScalarExpr::Literal(Value::Str(payload)) = &block.projections[0].expr else {
        panic!("payload");
    };
    assert_eq!(payload.as_ptr(), payload_pointer);
    assert_eq!(payload.len(), 4 * 1024 * 1024);
}

#[test]
fn sql_join_predicates_move_only_after_search_admission() {
    for admitted in [false, true] {
        let mut source = fixture(2, 1);
        let payload = "x".repeat(4 * 1024 * 1024);
        let pointer = payload.as_ptr();
        let SourcePlan::Join { on, .. } = &mut source else {
            panic!("join");
        };
        *on = Some(ScalarExpr::Binary {
            op: BinaryOp::Equal,
            lhs: Box::new(ScalarExpr::Literal(Value::Str(payload))),
            rhs: Box::new(ScalarExpr::Literal(Value::Str("y".into()))),
        });
        let budget = MemoryBudget::new(if admitted { 1024 * 1024 } else { 0 });
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut selected = None;
        let measured = allocation_counter::measure(|| {
            selected = Some(reordered_inner_join_source(
                &mut source,
                &[],
                &FunctionEstimates,
                &control,
            ));
        });
        assert!(
            measured.bytes_total < 16 * 1024,
            "admitted={admitted}: {measured:?}"
        );
        let selected = selected.unwrap();
        let selected = if admitted {
            Some(selected.unwrap().unwrap())
        } else {
            let crate::optimizer::OptimizerError::Expression(error) = selected.unwrap_err() else {
                panic!("admission error");
            };
            assert_eq!(error.sqlstate(), Some("53200"));
            None
        };
        let SourcePlan::Join {
            on: Some(ScalarExpr::Binary { lhs, .. }),
            ..
        } = selected.as_deref().unwrap_or(&source)
        else {
            panic!("predicate");
        };
        let ScalarExpr::Literal(Value::Str(payload)) = lhs.as_ref() else {
            panic!("literal");
        };
        assert_eq!(payload.as_ptr(), pointer);
        assert_eq!(payload.len(), 4 * 1024 * 1024);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn sql_join_materialization_preserves_nested_conjunct_order() {
    let predicate = |value: &str| ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(ScalarExpr::QualifiedColumn {
            qualifier: "s0".into(),
            column: "v".into(),
        }),
        rhs: Box::new(ScalarExpr::Literal(Value::Str(value.into()))),
    };
    let expected = vec![predicate("first"), predicate("second"), predicate("third")];
    let mut source = fixture(3, 1);
    let SourcePlan::Join { left, on, kind, .. } = &mut source else {
        panic!("join");
    };
    *kind = uqa_sql::ast::JoinKind::Inner;
    *on = Some(expected[2].clone());
    let SourcePlan::Join { on, kind, .. } = left.as_mut() else {
        panic!("left join");
    };
    *kind = uqa_sql::ast::JoinKind::Inner;
    *on = Some(ScalarExpr::And(vec![
        expected[0].clone(),
        ScalarExpr::And(vec![expected[1].clone()]),
        ScalarExpr::And(Vec::new()),
    ]));
    let selected = reordered_inner_join_source(
        &mut source,
        &[],
        &FunctionEstimates,
        &ProductionControl::uncontrolled(),
    )
    .unwrap()
    .unwrap();
    let SourcePlan::Join {
        on: Some(ScalarExpr::And(actual)),
        ..
    } = &*selected
    else {
        panic!("root conjunction");
    };
    assert_eq!(actual, &expected);
}

#[test]
fn inferred_join_output_retains_its_allocation_owner() {
    let mut source = fixture(2, 1);
    let expression = ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(ScalarExpr::QualifiedColumn {
            qualifier: "s0".into(),
            column: "v".repeat(4 * 1024 * 1024).into(),
        }),
        rhs: Box::new(ScalarExpr::QualifiedColumn {
            qualifier: "s1".into(),
            column: "v".into(),
        }),
    };
    let budget = MemoryBudget::new(16 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let selected =
        reordered_inner_join_source(&mut source, &[&expression], &FunctionEstimates, &control)
            .unwrap()
            .unwrap();
    assert!(
        budget.used() >= 4 * 1024 * 1024,
        "live inferred output lost its lease: {}",
        budget.used()
    );
    drop(selected);
    assert_eq!(budget.used(), 0);
}
