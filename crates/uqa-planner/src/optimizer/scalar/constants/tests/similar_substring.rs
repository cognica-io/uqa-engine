//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn optimize(source: &str) -> Result<ScalarExpr, SQLError> {
    let config =
        crate::optimizer::OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar);
    let uqa_sql::Statement::Select(mut query) = uqa_sql::compile(&format!("SELECT {source}"))
        .unwrap()
        .remove(0)
    else {
        panic!("SELECT")
    };
    let expression = uqa_sql::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar;
    let mut expression = uqa_sql::bind_type_introspection(expression, &RowSchema::default(), &[]);
    crate::optimizer::scalar::optimize_scalar_slot(&mut expression, &config)?;
    Ok(expression)
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
