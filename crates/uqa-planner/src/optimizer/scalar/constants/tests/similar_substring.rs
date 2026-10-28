//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn optimize(source: &str) -> Result<ScalarExpr, SQLError> {
    let config =
        crate::optimizer::OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar);
    let mut expression = expression(source);
    crate::optimizer::scalar::optimize_scalar_slot(&mut expression, &config)?;
    Ok(expression)
}

fn expression(source: &str) -> ScalarExpr {
    let uqa_sql::Statement::Select(mut query) = uqa_sql::compile(&format!("SELECT {source}"))
        .unwrap()
        .remove(0)
    else {
        panic!("SELECT")
    };
    let expression = uqa_sql::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar;
    uqa_sql::bind_type_introspection(expression, &RowSchema::default(), &[])
}

#[test]
fn similar_translation_planning_rejects_before_copying_large_operands() {
    use uqa_core::{memory::MemoryBudget, memory::ProductionControl, CancellationToken};

    for (bytes, cancelled) in [
        (0, None),
        (256, None),
        (usize::MAX, Some(false)),
        (usize::MAX, Some(true)),
    ] {
        let mut input = expression("substring($1 similar '%' escape '#')");
        let ScalarExpr::Func { args, .. } = &mut input else {
            panic!("selected substring call")
        };
        args[1] = ScalarExpr::Literal(Value::Str("%".repeat(4 * 1024 * 1024)));
        let memory = MemoryBudget::new(bytes);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if let Some(original_owner) = cancelled {
            if original_owner {
                original.cancel();
            } else {
                invoking.cancel();
            }
        }
        let mut config =
            crate::optimizer::OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar);
        config.production = ProductionControl::new(&memory, &original, &invoking);
        let mut result = None;
        let allocated = allocation_counter::measure(|| {
            result = Some(crate::optimizer::scalar::optimize_scalar_slot(
                &mut input, &config,
            ));
        });
        assert!(
            allocated.bytes_total < 16 * 1024,
            "bytes={bytes}, cancelled={cancelled:?}: {allocated:?}"
        );
        let error = result.unwrap().unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if cancelled.is_some() {
                "57014"
            } else {
                "53200"
            })
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn similar_translation_planning_keeps_lazy_errors_and_releases_workspace() {
    use uqa_core::{memory::MemoryBudget, memory::ProductionControl, CancellationToken};

    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let mut config =
        crate::optimizer::OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar);
    config.production = ProductionControl::new(&memory, &token, &token);
    config.coerced_conditionals = true;
    for (source, state) in [
        ("substring($1 similar '%' escape '#')", None),
        ("substring($1 similar '%' escape '##')", Some("22025")),
        (
            "substring($1 similar '#\"a#\"b#\"c#\"' escape '#')",
            Some("2200C"),
        ),
        ("substring(NULL similar '%' escape '##')", None),
        (
            "CASE WHEN true THEN $1 ELSE substring($1 similar '%' escape '##') END",
            None,
        ),
    ] {
        let mut input = expression(source);
        let result = crate::optimizer::scalar::optimize_scalar_slot(&mut input, &config);
        assert_eq!(
            result.as_ref().err().and_then(|error| error.sqlstate()),
            state,
            "{source}"
        );
        assert_eq!(memory.used(), 0, "{source}");
    }
    assert!(memory.peak() > 0);

    // An unexecutable SQL wrapper is not inlined. Its existing runtime permission error must remain deferred, even with an invalid constant escape and exhausted workspace.
    let full = memory.reserve(memory.limit()).unwrap();
    let mut input = optimize("substring($1 similar '%' escape '#')").unwrap();
    let ScalarExpr::Func { args, .. } = &mut input else {
        panic!("selected substring call")
    };
    args[2] = ScalarExpr::Literal(Value::Str("##".into()));
    assert_eq!(
        check_inlined_similar_substring(&input, None, None, None, &config.production)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    check_inlined_similar_substring(&input, Some(&DeniedBuiltin), None, None, &config.production)
        .unwrap();
    assert_eq!(memory.used(), memory.limit());
    drop(full);
    assert_eq!(memory.used(), 0);
}

#[test]
fn similar_substring_translation_follows_postgresql_inlining_while_planning() {
    // An invalid constant translation fails while planning, before any input row exists.
    for source in [
        "substring($1 similar '%' escape '##')",
        "substring($1 from '%' for '##')",
        "substring($1, '#\"a#\"b#\"c#\"', '#')",
        "substring('Thomas' similar '%' escape '##')",
    ] {
        let error = optimize(source).unwrap_err();
        assert!(
            matches!(error.sqlstate(), Some("22025" | "2200C")),
            "{source}: {error}"
        );
    }
    // A strict NULL constant folds the call without translating; a valid or non-constant translation and the positional form are left to execution.
    assert!(matches!(
        optimize("substring(NULL similar '%' escape '##')").unwrap(),
        ScalarExpr::Literal(Value::Null)
            | ScalarExpr::TypedLiteral {
                value: Value::Null,
                ..
            }
    ));
    for source in [
        "substring($1 similar '%' escape '#')",
        "substring($1 similar $2 escape '##')",
        "substring($1 similar '%' escape $2)",
        "substring($1 from 2 for 3)",
    ] {
        assert!(
            matches!(optimize(source).unwrap(), ScalarExpr::Func { .. }),
            "{source}"
        );
    }
}
