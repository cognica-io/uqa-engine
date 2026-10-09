//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18.6 rejects comparison keys during analysis, including empty inputs.

use super::*;

const COMPARISON_KEYS: &[(&str, &str, &str)] = &[
    ("SELECT DISTINCT 'ignored'::void", "equality", "void"),
    (
        "SELECT DISTINCT 'ignored'::void WHERE false",
        "equality",
        "void",
    ),
    ("SELECT DISTINCT '{}'::json", "equality", "json"),
    (
        "SELECT DISTINCT * FROM (SELECT 'ignored'::void AS v) AS s",
        "equality",
        "void",
    ),
    ("SELECT DISTINCT ON ('ignored'::void) 1", "equality", "void"),
    ("SELECT DISTINCT ON (1) 'ignored'::void", "equality", "void"),
    (
        "SELECT DISTINCT ON (v) 'ignored'::void AS v",
        "equality",
        "void",
    ),
    ("SELECT 'ignored'::void GROUP BY 1", "equality", "void"),
    ("SELECT 'ignored'::void AS v GROUP BY v", "equality", "void"),
    (
        "SELECT 'ignored'::void GROUP BY GROUPING SETS ((1), ())",
        "equality",
        "void",
    ),
    ("SELECT 'ignored'::void ORDER BY 1", "ordering", "void"),
    ("SELECT 'ignored'::void AS v ORDER BY v", "ordering", "void"),
    (
        "SELECT 1 ORDER BY 'ignored'::void LIMIT 0",
        "ordering",
        "void",
    ),
    (
        "SELECT 'left'::void UNION ALL SELECT 'right'::void ORDER BY 1",
        "ordering",
        "void",
    ),
    ("SELECT count(DISTINCT 'ignored'::void)", "equality", "void"),
    (
        "SELECT array_agg(1 ORDER BY 'ignored'::void)",
        "ordering",
        "void",
    ),
    (
        "SELECT row_number() OVER (PARTITION BY 'ignored'::void)",
        "equality",
        "void",
    ),
    (
        "SELECT row_number() OVER (ORDER BY 'ignored'::void)",
        "ordering",
        "void",
    ),
    (
        "SELECT 1 WINDOW unused AS (PARTITION BY 'ignored'::void)",
        "equality",
        "void",
    ),
    (
        "SELECT 1 WINDOW unused AS (ORDER BY 'ignored'::void)",
        "ordering",
        "void",
    ),
    (
        "SELECT 'left'::void UNION SELECT 'right'::void",
        "equality",
        "void",
    ),
];

#[test]
fn comparison_keys_require_operators_before_execution() {
    let mut mismatches = Vec::new();
    for &(sql, operator, ty) in COMPARISON_KEYS {
        for prepared in [false, true] {
            match analyze_key(sql, prepared) {
                Err(error)
                    if error.sqlstate() == Some("42883")
                        && error.to_string()
                            == format!(
                                "could not identify an {operator} operator for type {ty}"
                            )
                        && error.hint()
                            == (operator == "ordering").then_some(
                                "Use an explicit ordering operator or modify the query.",
                            ) => {}
                result => mismatches.push(format!("prepared={prepared} {sql}: {result:?}")),
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn output_types_without_comparison_remain_valid() {
    for sql in [
        "SELECT 'ignored'::void",
        "SELECT '{}'::json",
        "SELECT DISTINCT ON (1) 1, 'ignored'::void",
        "SELECT 'ignored'::void, 1 GROUP BY 2",
        "SELECT 'ignored'::void, 1 ORDER BY 2",
        "SELECT 'ignored'::void ORDER BY 1::integer",
        "SELECT 'ignored'::void ORDER BY 1 + 0",
        "SELECT 'left'::void UNION ALL SELECT 'right'::void",
    ] {
        for prepared in [false, true] {
            analyze_key(sql, prepared)
                .unwrap_or_else(|error| panic!("prepared={prepared} {sql}: {error}"));
        }
    }
}

fn analyze_key(sql: &str, prepared: bool) -> Result<(), SQLError> {
    let scopes = Scopes::default();
    if !prepared {
        return analyze(&scopes, sql);
    }
    let scope = Scope(&scopes);
    let context = scope.binding_context()?;
    let plan = UnifiedPlan::lower(crate::compile(sql)?.remove(0));
    crate::binding::infer_prepared_parameter_types(&NoRoutines, &plan, &[], &context).map(|_| ())
}
