//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::RangeSubtype, plan::ExpressionPlan, RowSchema};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn expression(sql: &str) -> ScalarExpr {
    let crate::Statement::Select(mut query) =
        crate::compile(&format!("SELECT {sql}")).unwrap().remove(0)
    else {
        panic!("SELECT expression")
    };
    ExpressionPlan::lower(query.projections.remove(0).expr).scalar
}

fn schema() -> RowSchema {
    RowSchema::with_types(
        ["small", "real", "n", "f", "a", "j", "c", "s", "b", "rng"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        vec![
            Some(ColumnType::SmallInteger),
            Some(ColumnType::Real),
            Some(ColumnType::Integer),
            Some(ColumnType::DoublePrecision),
            Some(ColumnType::Array(Box::new(ColumnType::Integer))),
            Some(ColumnType::Array(Box::new(ColumnType::Json))),
            Some(ColumnType::Regclass),
            Some(ColumnType::Text),
            Some(ColumnType::JsonB),
            Some(ColumnType::Range(RangeSubtype::Integer)),
        ],
    )
}

struct EmptyResolver;
impl FunctionTypeResolver for EmptyResolver {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&crate::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

#[test]
fn mutable_binding_preserves_scalar_semantics_and_exact_live_constructor_ownership() {
    let schema = schema();
    let resolver = EmptyResolver;
    let statements = [
        "pg_typeof(small)",
        "real::text",
        "-small",
        "COALESCE(NULL, small)",
        "jsonb_strip_nulls(b)",
        "ARRAY['2', small]",
        "'02/03/2020'::date",
        "CASE small WHEN '2' THEN real ELSE 7 END",
        "real + small",
        "n = f",
        "n IN ('2', 1, small)",
        "c NOT IN ('pg_class'::regclass, c, 'pg_type'::regclass)",
        "c = ANY(ARRAY['pg_class'::regclass])",
        "array_append(a, '2')",
        "a || '2'",
        "s || 'x'",
        "'abc' LIKE 'a%'",
        "'x' COLLATE \"C\"",
        "jsonb_extract_path(b, 'a', 'b')",
        "jsonb_extract_path(b, VARIADIC ARRAY['a'])",
        "array_sort(descending => 'true', \"array\" => j)",
        "lower(rng)",
        "b @> '{}'",
        "small ^ 2",
        "1 IN (missing, 2)",
    ];
    for sql in statements {
        let source = expression(sql);
        for resolver in [None, Some(&resolver as &dyn FunctionTypeResolver)] {
            let expected = match resolver {
                Some(resolver) => crate::bind_type_introspection_with_resolver(
                    source.clone(),
                    &schema,
                    &[],
                    resolver,
                ),
                None => crate::bind_type_introspection(source.clone(), &schema, &[]),
            };
            let budget = MemoryBudget::new(1 << 24);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            let mut output = None;
            let allocations = allocation_counter::measure(|| {
                let input = source.copy_for_rewrite(&control).unwrap();
                output = Some(
                    bind_type_introspection_owned(input, &schema, &[], resolver, &control).unwrap(),
                );
            });
            let output = output.unwrap();
            assert_eq!(*output, expected, "{sql}; resolver={}", resolver.is_some());
            assert_eq!(
                allocations.bytes_current,
                i64::try_from(budget.used()).unwrap(),
                "{sql}; resolver={}",
                resolver.is_some()
            );
            assert_eq!(budget.used(), output.reserved_bytes(), "{sql}");
            drop(output);
            assert_eq!(budget.used(), 0, "{sql}");
        }
    }
}

#[test]
fn mutable_binding_partial_admission_and_both_cancellations_release_private_inputs() {
    let schema = schema();
    let source = expression("jsonb_extract_path(b, 'a', 'b')");
    for mode in 0..4 {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let input = source.copy_for_rewrite(&control).unwrap();
        let sibling = budget
            .reserve(budget.available() - if mode == 1 { 64 } else { 0 })
            .unwrap();
        if mode == 2 {
            original.cancel();
        }
        if mode == 3 {
            invoking.cancel();
        }
        let error = bind_type_introspection_owned(input, &schema, &[], None, &control).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if mode < 2 { "53200" } else { "57014" })
        );
        assert_eq!(budget.used(), sibling.bytes());
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
}
