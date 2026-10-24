//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{scalar::PreparedExpressions, ScalarExpr};

#[test]
fn zero_argument_calls_keep_separate_descriptions_after_owner_moves() {
    let catalogs = ["first", "second"].map(|name| {
        let Statement::CreateFunction(mut definition) = uqa_sql::compile(&format!(
            "CREATE FUNCTION public.{name}() RETURNS integer LANGUAGE SQL AS 'SELECT 1'"
        ))
        .unwrap()
        .remove(0) else {
            panic!("function definition")
        };
        definition.object_id = Some([name.as_bytes()[0]; 16]);
        Catalog {
            function: Arc::new(SQLUserFunction::new(*definition, RoutineBody::Source)),
            ..Catalog::new()
        }
    });
    let expressions = catalogs
        .iter()
        .map(|catalog| {
            let binding = RoutineOverloadContext { catalog }
                .resolve_static_sql_function_match(
                    &catalog.function.def.name,
                    None,
                    &[],
                    &[],
                    false,
                )
                .unwrap()
                .unwrap()
                .binding();
            ScalarExpr::Func {
                name: binding.name.clone(),
                binding: Some(binding),
                args: Vec::new(),
                distinct: false,
                order_by: Vec::new(),
                order_syntax: uqa_sql::ast::FunctionOrderSyntax::default(),
                filter: None,
            }
        })
        .collect();
    let prepared = PreparedExpressions::scalars(expressions);
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let evaluate = |prepared: &PreparedExpressions<Vec<ScalarExpr>>| {
        for (expression, catalog) in prepared.iter().zip(&catalogs) {
            let ScalarExpr::Func {
                binding: Some(binding),
                ..
            } = expression
            else {
                panic!("bound function")
            };
            let current = || Some(catalog.scope.clone());
            let descriptions = prepared.calls().routine(binding).unwrap();
            let resolved = resolve_bound_inputs(
                catalog,
                &RoutineOverloadContext { catalog },
                Some(RoutineCallRetention {
                    descriptions,
                    scope: &current,
                    control,
                }),
                binding,
                &[],
            )
            .unwrap()
            .unwrap();
            assert!(Arc::ptr_eq(&resolved.function, &catalog.function));
            assert!(resolved.bound.is_empty());
            assert!(descriptions.get(binding, &catalog.scope).is_some());
        }
    };
    evaluate(&prepared);
    let moved = Box::new(prepared);
    for _ in 0..32 {
        evaluate(&moved);
    }
    for catalog in &catalogs {
        assert_eq!(catalog.lookups.load(Ordering::Relaxed), 1);
    }
    let cloned = moved.as_ref().clone();
    evaluate(&cloned);
    for catalog in &catalogs {
        assert_eq!(catalog.lookups.load(Ordering::Relaxed), 2);
    }
    drop((moved, cloned));
    assert_eq!(budget.used(), 0);
}
