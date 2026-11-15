//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::{
    ast::{Expr, FunctionBody, Statement},
    plan::UnifiedPlan,
    routines::{CompiledFunctionBody, RoutineBody, SQLUserFunction},
};

fn cast(oid: u32) -> Expr {
    Expr::Cast {
        implicit: false,
        expr: Box::new(Expr::Cast {
            implicit: false,
            expr: Box::new(Expr::Literal(uqa_core::Value::Null)),
            ty: format!("enum#{oid}"),
        }),
        ty: "text".into(),
    }
}

#[test]
fn native_routine_types_validate_original_defaults_syntax_and_retained_plans() {
    for location in 0..3 {
        for oid in [25_000, 25_002] {
            let mut snapshot = typed_snapshot();
            snapshot.tables.clear();
            let Statement::CreateFunction(mut definition) = uqa_sql::compile(
                "CREATE FUNCTION public.saved(n text DEFAULT NULL) RETURNS text LANGUAGE SQL AS 'unexamined source text'",
            ).unwrap().remove(0) else { panic!("routine definition") };
            let statement = |oid| Statement::Values {
                rows: vec![vec![cast(oid)]],
            };
            let body = if location == 0 {
                definition.params[0].default = Some(cast(oid));
                RoutineBody::Source
            } else {
                definition.body = FunctionBody::Statements(vec![statement(if location == 1 {
                    oid
                } else {
                    25_000
                })]);
                RoutineBody::Bound(
                    CompiledFunctionBody::SQL(vec![UnifiedPlan::lower(statement(
                        if location == 2 { oid } else { 25_000 },
                    ))])
                    .shared(),
                )
            };
            RetainedValue::make_mut(&mut snapshot.definitions.sql_user_functions)
                .insert(
                    "public.saved".into(),
                    vec![SQLUserFunction::new(*definition, body).shared()],
                )
                .unwrap();
            let memory = MemoryBudget::new(1 << 20);
            let result = validate_types(&snapshot, &memory, &CancellationToken::new());
            assert_eq!(memory.used(), 0);
            assert_eq!(
                result.is_ok(),
                oid == 25_000,
                "location={location}, oid={oid}, result={result:?}"
            );
        }
    }
}
