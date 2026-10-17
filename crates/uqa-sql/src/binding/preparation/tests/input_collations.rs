//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Each analyzed call records `PostgreSQL`'s `inputcollid` on the binding it keeps.

use super::*;

fn context() -> BindingContext<'static> {
    let crate::Statement::CreateTable(mut table) =
        crate::compile("CREATE TABLE t (c text, p text, d text, n integer)")
            .unwrap()
            .remove(0)
    else {
        panic!("table declaration")
    };
    table.columns[0].collation_oid = Some(950);
    table.columns[1].collation_oid = Some(951);
    BindingContext {
        catalog: crate::binding::fixture::catalog(BTreeMap::from([(
            RelationIdentity::new("public", "t"),
            crate::binding::fixture::table_definition(table.columns),
        )])),
        ..assignment_context()
    }
}

/// Every call of the analyzed statement with the input collation its binding records, in visiting order. COLLATE clauses select a collation rather than consume one.
fn input_collations(sql: &str) -> Vec<(String, Option<u32>)> {
    let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
    read_prepared_inputs(&NoRoutines, &mut plan, &[], &context(), None)
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    let mut calls = Vec::new();
    plan.rewrite_scalar_expressions(&mut |expression| {
        if let ScalarExpr::Func { name, binding, .. } = expression {
            let binding = binding
                .as_ref()
                .unwrap_or_else(|| panic!("{sql}: {name} keeps no binding"));
            if matches!(
                binding.dispatch,
                Some(crate::ast::FunctionDispatch::Collate { .. })
            ) {
                return;
            }
            calls.push((
                name.clone(),
                binding
                    .input_collation
                    .map(crate::catalog::collations::CollationSelection::oid),
            ));
        }
    });
    calls
}

#[test]
fn calls_record_the_merged_collation_of_their_inputs() {
    for (sql, expected) in [
        // A column's implicit collation, an explicit COLLATE and the default collation of a literal.
        ("SELECT lower(c) FROM t", vec![("lower", Some(950))]),
        (
            "SELECT lower(c COLLATE \"unicode\") FROM t",
            vec![("lower", Some(963))],
        ),
        ("SELECT upper(d) FROM t", vec![("upper", Some(100))]),
        ("SELECT initcap('a')", vec![("initcap", Some(100))]),
        // A non-default implicit collation beats the default one.
        (
            "SELECT casefold(d || c) FROM t",
            vec![("||", Some(950)), ("casefold", Some(950))],
        ),
        // Conflicting implicit collations leave the selection indeterminate, while an explicit one resolves them.
        (
            "SELECT lower(c || p) FROM t",
            vec![("||", Some(0)), ("lower", Some(0))],
        ),
        ("SELECT c ~ p FROM t", vec![("~", Some(0))]),
        (
            "SELECT c ILIKE (p COLLATE \"ucs_basic\") FROM t",
            vec![("ILIKE", Some(962))],
        ),
        (
            "SELECT c NOT LIKE 'a%' FROM t",
            vec![("NOT LIKE", Some(950))],
        ),
        // Nested calls derive their own selections.
        (
            "SELECT upper(lower(c) COLLATE \"POSIX\") FROM t",
            vec![("lower", Some(950)), ("upper", Some(951))],
        ),
        // Regular-expression functions without a registered signature keep a selection by name.
        (
            "SELECT regexp_like(p, 'a') FROM t",
            vec![("regexp_like", Some(951))],
        ),
        (
            "SELECT regexp_replace(d, 'a', 'b') FROM t",
            vec![("regexp_replace", Some(100))],
        ),
        // A call without collatable inputs records nothing.
        ("SELECT abs(n) FROM t", vec![("abs", None)]),
    ] {
        let expected: Vec<_> = expected
            .into_iter()
            .map(|(name, collation)| (name.to_owned(), collation))
            .collect();
        assert_eq!(input_collations(sql), expected, "{sql}");
    }
}

#[test]
fn calls_in_filters_and_commands_record_their_selections() {
    assert_eq!(
        input_collations("SELECT n FROM t WHERE upper(p) = 'A'"),
        [("upper".to_owned(), Some(951))]
    );
    assert_eq!(
        input_collations("UPDATE t SET d = initcap(c COLLATE \"pg_c_utf8\") WHERE d ILIKE 'x'"),
        [
            ("initcap".to_owned(), Some(811)),
            ("ILIKE".to_owned(), Some(100))
        ]
    );
}
