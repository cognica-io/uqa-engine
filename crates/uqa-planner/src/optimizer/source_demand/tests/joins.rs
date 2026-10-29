//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

fn join_fixture(names: Vec<String>, using: bool) -> (SourcePlan, AnalyzedSource) {
    let mut query = query(if using {
        "SELECT 1 FROM l JOIN r USING (id)"
    } else {
        "SELECT 1 FROM l JOIN r ON l.id = r.id"
    });
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block");
    };
    let mut source = block.from.take().unwrap();
    if let SourcePlan::Join {
        using: Some(using), ..
    } = &mut source
    {
        using.columns = names.clone();
    }
    let types = vec![Some(ColumnType::Integer); names.len()];
    let left = RowSchema::with_qualified_types("l", names.clone(), types.clone());
    let right = RowSchema::with_qualified_types("r", names, types);
    (
        source,
        AnalyzedSource {
            schema: RowSchema::default(),
            children: Some(Box::new((
                AnalyzedSource {
                    schema: left,
                    children: None,
                },
                AnalyzedSource {
                    schema: right,
                    children: None,
                },
            ))),
        },
    )
}

fn bounded_join(using: bool, cancelled: Option<bool>) {
    let names = if using {
        vec!["x".repeat(64 * 1024)]
    } else {
        std::iter::once("id".into())
            .chain((1..1024).map(|index| format!("column_{index:049}")))
            .collect()
    };
    let (mut source, analysis) = join_fixture(names, using);
    let before = serde_json::to_value(&source).unwrap();
    let parent = MemoryBudget::new(4096);
    let sibling = parent.reserve(512).unwrap();
    let memory = parent.child(if using { 1024 } else { 128 });
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    match cancelled {
        Some(true) => original.cancel(),
        Some(false) => invoking.cancel(),
        None => {}
    }
    let control = ProductionControl::new(&memory, &original, &invoking);
    let mut config = OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control);
    config.production = control;
    let context = Context {
        config: &config,
        aggregates: &|_: &str| false,
        ctes: None,
    };
    let demanded = Demand::new(control);
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(context.join(&mut source, &analysis, &demanded, &[]));
    });
    assert!(allocation.bytes_total < 8192, "{allocation:?}");
    assert_eq!(
        result.unwrap().unwrap_err().sqlstate(),
        Some(if cancelled.is_some() {
            "57014"
        } else {
            "53200"
        })
    );
    assert_eq!(memory.used(), 0);
    assert_eq!(parent.used(), sibling.bytes());
    assert_eq!(serde_json::to_value(&source).unwrap(), before);
}

#[test]
fn positional_join_rejects_before_copying_wide_input_schemas() {
    bounded_join(false, None);
}

#[test]
fn positional_join_using_rejects_before_copying_wide_column_names() {
    bounded_join(true, None);
}

#[test]
fn positional_join_observes_both_cancellations_before_input_construction() {
    for original in [false, true] {
        bounded_join(false, Some(original));
    }
}
