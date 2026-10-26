//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    expr_contains_volatile_function, FunctionBinding, FunctionVolatility, QueryPlan, SQLError,
    ScalarExpr, VolatilityCatalog,
};

struct EmptyCatalog;
impl VolatilityCatalog for EmptyCatalog {
    fn host_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
        None
    }
    fn routine_volatilities(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
    ) -> Option<Vec<FunctionVolatility>> {
        None
    }
    fn view_query(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
        Ok(None)
    }
}

#[test]
fn external_cte_names_do_not_leak_into_catalog_view_definitions() {
    struct Catalog;
    impl VolatilityCatalog for Catalog {
        fn host_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
            None
        }
        fn routine_volatilities(
            &self,
            _: &str,
            _: Option<&FunctionBinding>,
        ) -> Option<Vec<FunctionVolatility>> {
            None
        }
        fn view_query(&self, name: &str) -> Result<Option<QueryPlan>, SQLError> {
            let sql = if name == "visible_view" {
                "SELECT * FROM shadowed"
            } else {
                "SELECT nextval('ticks')"
            };
            let crate::plan::UnifiedPlan::Query(query) =
                crate::plan::UnifiedPlan::lower(crate::compile(sql)?.remove(0))
            else {
                unreachable!()
            };
            Ok(Some(*query))
        }
    }
    for (sql, expected) in [
        ("SELECT (SELECT * FROM shadowed)", false),
        ("SELECT (SELECT (SELECT * FROM shadowed))", false),
        ("SELECT (SELECT * FROM visible_view)", true),
        ("SELECT (WITH local AS (SELECT nextval('ticks')) SELECT * FROM local)", true),
        ("SELECT (WITH RECURSIVE local AS (SELECT * FROM later),later AS (VALUES(1)) SELECT * FROM local)", false),
    ] {
        let crate::plan::UnifiedPlan::Query(query) = crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0)) else { unreachable!() };
        let crate::plan::RelationalPlan::QueryBlock(block) = query.root else { unreachable!() };
        assert_eq!(super::scalar_in_scope_contains_volatile_function(&Catalog, &block.projections[0].expr, &block.subqueries, &|name| name == "shadowed").unwrap(), expected, "{sql}");
    }
}

#[test]
fn query_volatility_keeps_lexical_ctes_separate_from_catalog_views() {
    struct Catalog(std::cell::Cell<usize>);
    impl VolatilityCatalog for Catalog {
        fn host_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
            None
        }
        fn routine_volatilities(
            &self,
            _: &str,
            _: Option<&FunctionBinding>,
        ) -> Option<Vec<FunctionVolatility>> {
            None
        }
        fn view_query(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
            self.0.set(self.0.get() + 1);
            let crate::plan::UnifiedPlan::Query(query) = crate::plan::UnifiedPlan::lower(
                crate::compile("SELECT nextval('ticks')").unwrap().remove(0),
            ) else {
                panic!("query")
            };
            Ok(Some(*query))
        }
    }
    for (sql, expected) in [
        ("WITH shadowed AS (VALUES(1)) SELECT * FROM shadowed", false),
        (
            "WITH shadowed AS (VALUES(1)) SELECT (SELECT * FROM shadowed)",
            false,
        ),
        (
            "WITH shadowed AS (VALUES(1)) SELECT * FROM (SELECT * FROM shadowed) nested",
            false,
        ),
        (
            "WITH shadowed AS (VALUES(1)), other AS (SELECT * FROM shadowed) SELECT * FROM other",
            false,
        ),
        (
            "WITH shadowed AS (VALUES(1)) SELECT * FROM public.shadowed",
            true,
        ),
    ] {
        let crate::plan::UnifiedPlan::Query(query) =
            crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let catalog = Catalog(std::cell::Cell::new(0));
        assert_eq!(
            super::query_contains_volatile_function(&catalog, &query).unwrap(),
            expected,
            "{sql}"
        );
        assert_eq!(catalog.0.get(), usize::from(expected), "{sql}");
    }
}
use crate::ast::{FrameExclusion, FrameMode};
use crate::{ScalarFrameBound, ScalarWindowFrame, ScalarWindowSpec};

#[test]
fn collation_volatility_follows_only_its_operand() {
    struct Shadowed;
    impl VolatilityCatalog for Shadowed {
        fn host_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
            Some(FunctionVolatility::Volatile)
        }
        fn routine_volatilities(
            &self,
            _: &str,
            _: Option<&FunctionBinding>,
        ) -> Option<Vec<FunctionVolatility>> {
            unreachable!("host function wins ordinary calls")
        }
        fn view_query(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
            unreachable!("no view")
        }
    }
    for (input, expected) in [
        ("'x' COLLATE \"C\"", false),
        ("nextval('effect')::text COLLATE \"C\"", true),
        ("\"COLLATE\"('x')", true),
        ("true IS TRUE", false),
        ("(nextval('effect') > 0) IS NOT FALSE", true),
        ("\"IS TRUE\"(true)", true),
        ("(ARRAY[1,2])[1]", false),
        ("(ARRAY[1,2])[:1]", false),
        ("(ARRAY[1,2])[nextval('effect')]", true),
        ("(ARRAY[1,2])[:nextval('effect')]", true),
        ("slice(ARRAY[1,2], 1, 2)", true),
        ("subscript(ARRAY[1,2], 1)", true),
    ] {
        let crate::Statement::Select(mut query) = crate::compile(&format!("SELECT {input}"))
            .unwrap()
            .remove(0)
        else {
            panic!("SELECT")
        };
        let scalar = crate::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar;
        assert_eq!(
            expr_contains_volatile_function(&Shadowed, &scalar),
            expected,
            "{input}"
        );
    }
    let binding =
        FunctionBinding::dispatched(crate::ast::FunctionDispatch::Collate { oid: Some(950) });
    assert_eq!(
        super::builtin_function_volatility("nextval", Some(&binding), 2),
        FunctionVolatility::Immutable
    );
}

#[test]
fn builtin_constants_preserve_postgresql_statement_and_session_inputs() {
    // Checked against PostgreSQL 18.4 pg_proc.provolatile before enabling bound built-in constant folding.
    for name in [
        "concat",
        "concat_ws",
        "format",
        "pg_typeof",
        "row_to_json",
        "to_json",
        "to_jsonb",
        "json_build_object",
        "jsonb_build_array",
        "to_char",
        "to_date",
        "to_number",
        "has_table_privilege",
        "has_column_privilege",
    ] {
        assert_eq!(
            super::builtin_function_volatility(name, None, 2),
            FunctionVolatility::Stable,
            "{name}"
        );
    }
    for name in ["repeat", "upper"] {
        assert_eq!(
            super::builtin_function_volatility(name, None, 2),
            FunctionVolatility::Immutable,
            "{name}"
        );
    }
    assert_eq!(
        super::builtin_function_volatility("nextval", None, 1),
        FunctionVolatility::Volatile
    );
}

#[test]
fn sequence_introspection_volatility_matches_postgresql() {
    for (name, expected) in [
        ("pg_get_sequence_data", FunctionVolatility::Volatile),
        ("pg_sequence_last_value", FunctionVolatility::Volatile),
        ("pg_sequence_parameters", FunctionVolatility::Stable),
    ] {
        for qualified in [name.to_string(), format!("pg_catalog.{name}")] {
            assert_eq!(
                super::function_volatility(&EmptyCatalog, &qualified, 1),
                expected,
                "{qualified}"
            );
        }
    }
}

#[test]
fn timestamp_zone_volatility_preserves_session_dependence() {
    assert_eq!(
        super::builtin_function_volatility("timezone", None, 1),
        FunctionVolatility::Stable
    );
    assert_eq!(
        super::builtin_function_volatility("timezone", None, 2),
        FunctionVolatility::Immutable
    );
}

#[test]
fn volatility_inspection_includes_window_frame_expressions() {
    let expression = ScalarExpr::WindowCall {
        name: "sum".into(),
        args: vec![ScalarExpr::Column("amount".into())],
        spec: ScalarWindowSpec {
            definition: None,
            partition_by: Vec::new(),
            order_by: Vec::new(),
            frame: Some(ScalarWindowFrame {
                mode: FrameMode::Rows,
                start: ScalarFrameBound::Preceding(Box::new(ScalarExpr::Func {
                    order_syntax: crate::ast::FunctionOrderSyntax::Ordinary,
                    name: "random".into(),
                    binding: None,
                    args: Vec::new(),
                    distinct: false,
                    order_by: Vec::new(),
                    filter: None,
                })),
                end: ScalarFrameBound::CurrentRow,
                between: true,
                exclusion: FrameExclusion::NoOthers,
            }),
        },
        filter: None,
        modifiers: crate::ast::WindowCallModifiers::default(),
    };
    assert!(expr_contains_volatile_function(&EmptyCatalog, &expression));
}
#[test]
fn scalar_plan_volatility_inspects_its_subquery_body() {
    for (sql, expected) in [
        ("SELECT (SELECT 1 / 0)", false),
        ("SELECT (SELECT nextval('ticks'))", true),
        ("SELECT (VALUES ((SELECT 1 / 0)))", false),
        ("SELECT (VALUES ((SELECT nextval('ticks'))))", true),
        (
            "SELECT (SELECT v FROM (VALUES ((SELECT 1 / 0))) t(v))",
            false,
        ),
        (
            "SELECT (SELECT v FROM (VALUES ((SELECT nextval('ticks')))) t(v))",
            true,
        ),
        (
            "SELECT EXISTS (SELECT 1 FROM a JOIN b ON (SELECT true))",
            false,
        ),
        (
            "SELECT EXISTS (SELECT 1 FROM a JOIN b ON (SELECT nextval('ticks') > 0))",
            true,
        ),
        (
            "SELECT (SELECT n FROM generate_series(1, (SELECT 1 / 0)) n)",
            false,
        ),
        (
            "SELECT (SELECT n FROM generate_series(1, (SELECT nextval('ticks'))) n)",
            true,
        ),
        (
            "SELECT 1 IN (VALUES (1) UNION ALL VALUES (2) LIMIT (SELECT 1 / 0))",
            false,
        ),
        (
            "SELECT 1 IN (VALUES (1) UNION ALL VALUES (2) LIMIT (SELECT nextval('ticks')))",
            true,
        ),
    ] {
        let crate::plan::UnifiedPlan::Query(query) =
            crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("query");
        };
        let crate::plan::RelationalPlan::QueryBlock(mut block) = query.root else {
            panic!("query block");
        };
        let plan = crate::plan::ExpressionPlan {
            scalar: block.projections.remove(0).expr,
            subqueries: block.subqueries,
        };
        assert_eq!(
            super::expression_plan_contains_volatile_function(&EmptyCatalog, &plan).unwrap(),
            expected,
            "{sql}"
        );
    }
}

#[test]
fn scalar_volatility_follows_only_referenced_arena_entries() {
    let crate::plan::UnifiedPlan::Query(query) = crate::plan::UnifiedPlan::lower(
        crate::compile("SELECT (SELECT 1 / 0), (SELECT nextval('ticks'))")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    let crate::plan::RelationalPlan::QueryBlock(block) = query.root else {
        panic!("block")
    };
    for (projection, expected) in block.projections.iter().zip([false, true]) {
        assert_eq!(
            super::scalar_with_subqueries_contains_volatile_function(
                &EmptyCatalog,
                &projection.expr,
                &block.subqueries,
            )
            .unwrap(),
            expected,
        );
    }
    assert!(super::scalar_with_subqueries_contains_volatile_function(
        &EmptyCatalog,
        &ScalarExpr::ScalarSubquery(block.subqueries.len()),
        &block.subqueries,
    )
    .unwrap());
}

#[test]
fn scalar_subquery_volatility_propagates_catalog_failures() {
    struct FailedCatalog;
    impl VolatilityCatalog for FailedCatalog {
        fn host_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
            None
        }
        fn routine_volatilities(
            &self,
            _: &str,
            _: Option<&FunctionBinding>,
        ) -> Option<Vec<FunctionVolatility>> {
            None
        }
        fn view_query(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
            Err(SQLError::Internal("volatility catalog failure".into()))
        }
    }
    let crate::plan::UnifiedPlan::Query(query) = crate::plan::UnifiedPlan::lower(
        crate::compile("SELECT 1 FROM inaccessible")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    let error = super::scalar_with_subqueries_contains_volatile_function(
        &FailedCatalog,
        &ScalarExpr::ScalarSubquery(0),
        &[*query],
    )
    .unwrap_err();
    assert!(error.to_string().contains("volatility catalog failure"));
}

#[test]
fn unified_plan_volatility_borrows_large_payloads_without_allocating_plan_scratch() {
    for sql in [
        "SELECT 'payload'",
        "INSERT INTO t VALUES ('payload')",
        "UPDATE t SET v='payload' RETURNING v",
        "DELETE FROM t WHERE v='payload' RETURNING v",
        "CREATE TABLE t AS SELECT 'payload'",
        "EXPLAIN SELECT 'payload'",
        "PREPARE p AS SELECT 'payload'",
    ] {
        let mut plan = crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let mut replaced = 0;
        plan.rewrite_scalar_expressions(&mut |expression| {
            if let ScalarExpr::Literal(uqa_core::Value::Str(text)) = expression {
                if text == "payload" {
                    *text = "x".repeat(4 * 1024 * 1024);
                    replaced += 1;
                }
            }
        });
        assert_eq!(replaced, 1, "{sql}");
        let allocated = allocation_counter::measure(|| {
            assert!(!super::unified_plan_contains_volatile_function(
                &EmptyCatalog,
                &plan
            ));
        });
        assert_eq!(allocated.count_total, 0, "{sql}: {allocated:?}");
    }
    let rows = std::iter::repeat_n("(1)", 4096)
        .collect::<Vec<_>>()
        .join(",");
    for sql in [
        format!("VALUES {rows}"),
        format!("INSERT INTO t VALUES {rows}"),
    ] {
        let plan = crate::plan::UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0));
        let allocated = allocation_counter::measure(|| {
            assert!(!super::unified_plan_contains_volatile_function(
                &EmptyCatalog,
                &plan
            ));
        });
        assert_eq!(allocated.count_total, 0, "{allocated:?}");
    }
}

#[test]
fn borrowed_volatility_inspection_keeps_query_mutation_and_utility_children() {
    for sql in [
        "SELECT nextval('s')",
        "SELECT 1 FROM (SELECT nextval('s')) x",
        "WITH x AS (SELECT nextval('s')) SELECT 1",
        "SELECT (SELECT nextval('s'))",
        "SELECT sum(1) OVER (ORDER BY 1 ROWS nextval('s') PRECEDING)",
        "INSERT INTO t VALUES (nextval('s'))",
        "INSERT INTO t VALUES (1) ON CONFLICT (v) DO UPDATE SET v=nextval('s')",
        "UPDATE t SET v=1 FROM (SELECT nextval('s')) x",
        "UPDATE t SET a[nextval('s')]=1",
        "DELETE FROM t RETURNING nextval('s')",
        "MERGE INTO t USING (VALUES (1)) s(v) ON true WHEN MATCHED THEN UPDATE SET v=nextval('s')",
        "CREATE VIEW v AS SELECT nextval('s')",
        "CREATE MATERIALIZED VIEW v AS SELECT nextval('s')",
        "CREATE TABLE t AS SELECT nextval('s')",
        "DECLARE c CURSOR FOR SELECT nextval('s')",
        "EXPLAIN SELECT nextval('s')",
        "PREPARE p AS SELECT nextval('s')",
        "EXECUTE p(nextval('s'))",
        "CALL p(nextval('s'))",
    ] {
        let plan = crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        assert!(
            super::unified_plan_contains_volatile_function(&EmptyCatalog, &plan),
            "{sql}"
        );
        let pure = sql.replace("nextval('s')", "1");
        let plan = crate::plan::UnifiedPlan::lower(crate::compile(&pure).unwrap().remove(0));
        assert!(
            !super::unified_plan_contains_volatile_function(&EmptyCatalog, &plan),
            "{pure}"
        );
    }
}

struct BorrowedViews {
    definitions: Vec<(&'static str, QueryPlan)>,
    reads: std::cell::RefCell<Vec<&'static str>>,
}

impl BorrowedViews {
    fn query(sql: &str) -> QueryPlan {
        let crate::plan::UnifiedPlan::Query(query) =
            crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("query expected")
        };
        *query
    }

    fn new(payload_bytes: usize) -> Self {
        let mut leaf = Self::query("SELECT 'payload'");
        leaf.rewrite_scalar_expressions(&mut |expression| {
            if let ScalarExpr::Literal(uqa_core::Value::Str(text)) = expression {
                *text = "x".repeat(payload_bytes);
            }
        });
        Self {
            definitions: vec![
                ("root", Self::query("SELECT * FROM leaf")),
                ("leaf", leaf),
                ("cycle_a", Self::query("SELECT * FROM cycle_b")),
                ("cycle_b", Self::query("SELECT * FROM cycle_a")),
                ("effect", Self::query("SELECT nextval('ticks')")),
            ],
            reads: std::cell::RefCell::new(Vec::with_capacity(32)),
        }
    }
}

impl VolatilityCatalog for BorrowedViews {
    fn host_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
        None
    }

    fn routine_volatilities(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
    ) -> Option<Vec<FunctionVolatility>> {
        None
    }

    fn view_query(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
        panic!("semantic inspection must borrow the retained definition")
    }

    fn with_view_query(
        &self,
        name: &str,
        read: &mut dyn FnMut(&QueryPlan) -> Result<bool, SQLError>,
    ) -> Result<Option<bool>, SQLError> {
        if name == "broken" {
            self.reads.borrow_mut().push("broken");
            return Err(SQLError::Internal("view catalog failure".into()));
        }
        self.definitions
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(key, definition)| {
                self.reads.borrow_mut().push(key);
                read(definition)
            })
            .transpose()
    }
}

#[test]
fn transitive_view_inspection_borrows_payloads_and_revisits_independent_branches() {
    for (sql, expected, reads) in [
        ("SELECT * FROM root", false, vec!["root", "leaf"]),
        (
            "SELECT * FROM root a CROSS JOIN root b",
            false,
            vec!["root", "leaf", "root", "leaf"],
        ),
        ("SELECT * FROM cycle_a", false, vec!["cycle_a", "cycle_b"]),
        (
            "SELECT * FROM effect CROSS JOIN broken",
            true,
            vec!["effect"],
        ),
    ] {
        let query = BorrowedViews::query(sql);
        let small = BorrowedViews::new(1);
        let large = BorrowedViews::new(4 * 1024 * 1024);
        let measure = |catalog: &BorrowedViews| {
            let allocations = allocation_counter::measure(|| {
                assert_eq!(
                    super::query_contains_volatile_function(catalog, &query).unwrap(),
                    expected,
                    "{sql}"
                );
            });
            assert_eq!(*catalog.reads.borrow(), reads, "{sql}");
            (allocations.count_total, allocations.bytes_total)
        };
        assert_eq!(measure(&small), measure(&large), "{sql}");
    }
}

#[test]
fn borrowed_view_inspection_propagates_the_first_catalog_error() {
    let catalog = BorrowedViews::new(1);
    let query = BorrowedViews::query("SELECT * FROM broken CROSS JOIN effect");
    let error = super::query_contains_volatile_function(&catalog, &query).unwrap_err();
    assert_eq!(error.to_string(), "internal error: view catalog failure");
    assert_eq!(*catalog.reads.borrow(), ["broken"]);
}

#[test]
fn builtin_volatility_inspection_borrows_normalized_names() {
    for (name, expected) in [
        ("length", FunctionVolatility::Immutable),
        ("pg_catalog.length", FunctionVolatility::Immutable),
        ("count", FunctionVolatility::Immutable),
        ("pg_catalog.count", FunctionVolatility::Immutable),
        ("random", FunctionVolatility::Volatile),
        ("pg_catalog.nextval", FunctionVolatility::Volatile),
        ("concat", FunctionVolatility::Stable),
        ("pg_catalog.concat", FunctionVolatility::Stable),
    ] {
        assert_eq!(
            super::function_volatility(&EmptyCatalog, name, 1),
            expected,
            "{name}"
        );
        let allocations = allocation_counter::measure(|| {
            assert_eq!(super::function_volatility(&EmptyCatalog, name, 1), expected);
        });
        assert_eq!(allocations.count_total, 0, "{name}: {allocations:?}");
    }
}

#[test]
fn qualified_variadic_volatility_matches_postgresql_without_copying_names() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("variadic.pg18.json")).unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let expected = match case["volatility"].as_str().unwrap() {
            "s" => FunctionVolatility::Stable,
            "i" => FunctionVolatility::Immutable,
            other => panic!("unexpected reference volatility {other}"),
        };
        for name in [name.to_string(), format!("pg_catalog.{name}")] {
            assert_eq!(
                super::function_volatility(&EmptyCatalog, &name, 2),
                expected,
                "{name}"
            );
            let allocations = allocation_counter::measure(|| {
                assert_eq!(
                    super::function_volatility(&EmptyCatalog, &name, 2),
                    expected,
                    "{name}"
                );
                assert!(crate::expr::variadic_any::is_variadic_any(&name));
            });
            assert_eq!(allocations.count_total, 0, "{name}: {allocations:?}");
        }
    }
}
