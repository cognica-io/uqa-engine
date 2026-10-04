//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn columns(sql: &str) -> Vec<ColumnDef> {
    let uqa_sql::Statement::CreateTable(table) = uqa_sql::compile(sql).unwrap().remove(0) else {
        unreachable!()
    };
    table.columns
}

fn column(name: &str) -> ScalarExpr {
    ScalarExpr::Column(name.into())
}

fn int(value: i64) -> ScalarExpr {
    ScalarExpr::Literal(Value::Int(value))
}

fn equal(lhs: ScalarExpr, rhs: ScalarExpr) -> ScalarExpr {
    ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    }
}

fn within(expr: ScalarExpr, list: Vec<ScalarExpr>) -> ScalarExpr {
    ScalarExpr::InList {
        expr: Box::new(expr),
        list,
        negated: false,
    }
}

fn keyed(
    filter: &ScalarExpr,
    params: &[SQLParam],
    definitions: &[ColumnDef],
    maps: bool,
) -> Option<Vec<DocId>> {
    key_candidates(
        filter,
        params,
        IdentityColumns::new(definitions, maps, |name| name),
    )
}

#[test]
fn an_integer_primary_key_that_names_identities_selects_them() {
    let definitions = columns("CREATE TABLE t (id integer PRIMARY KEY, v text)");
    assert_eq!(
        keyed(&equal(column("id"), int(7)), &[], &definitions, true),
        Some(vec![7])
    );
    assert_eq!(
        keyed(&equal(int(7), column("id")), &[], &definitions, true),
        Some(vec![7])
    );
    // A parameter and arithmetic on it read no row.
    let shifted = equal(
        column("id"),
        ScalarExpr::Binary {
            op: BinaryOp::Subtract,
            lhs: Box::new(ScalarExpr::Param(1)),
            rhs: Box::new(int(1)),
        },
    );
    assert_eq!(
        keyed(
            &shifted,
            &[SQLParam::scalar(Value::Int(10))],
            &definitions,
            true
        ),
        Some(vec![9])
    );
    assert_eq!(
        keyed(
            &within(column("id"), vec![int(5), int(2), int(5)]),
            &[],
            &definitions,
            true
        ),
        Some(vec![2, 5])
    );
    let both = ScalarExpr::And(vec![
        within(column("id"), vec![int(1), int(2), int(3)]),
        equal(column("v"), ScalarExpr::Literal(Value::Str("x".into()))),
        within(column("id"), vec![int(3), int(4)]),
    ]);
    assert_eq!(keyed(&both, &[], &definitions, true), Some(vec![3]));
}

#[test]
fn a_key_value_that_names_no_identity_leaves_the_filter_unrestricted() {
    let definitions = columns("CREATE TABLE t (id integer PRIMARY KEY, v text)");
    // A negative key and a key past the identity range belong to rows whose identities they do not name.
    for value in [
        Value::Int(-5),
        Value::Int(1 << 62),
        Value::Null,
        Value::Str("7".into()),
    ] {
        assert_eq!(
            keyed(
                &equal(column("id"), ScalarExpr::Literal(value.clone())),
                &[],
                &definitions,
                true
            ),
            None,
            "{value:?}"
        );
    }
    assert_eq!(
        keyed(
            &within(column("id"), vec![int(1), int(-1)]),
            &[],
            &definitions,
            true
        ),
        None
    );
    // A table that does not keep its keys as identities, a composite key, a non-integer key and a key compared with a column name nothing.
    assert_eq!(
        keyed(&equal(column("id"), int(7)), &[], &definitions, false),
        None
    );
    let composite = columns("CREATE TABLE t (a integer, b integer, PRIMARY KEY (a, b))");
    assert_eq!(
        keyed(&equal(column("a"), int(7)), &[], &composite, true),
        None
    );
    let text = columns("CREATE TABLE t (id text PRIMARY KEY)");
    assert_eq!(
        keyed(
            &equal(column("id"), ScalarExpr::Literal(Value::Str("7".into()))),
            &[],
            &text,
            true
        ),
        None
    );
    assert_eq!(
        keyed(&equal(column("id"), column("v")), &[], &definitions, true),
        None
    );
    assert_eq!(
        keyed(
            &ScalarExpr::Or(vec![
                equal(column("id"), int(1)),
                equal(column("id"), int(2))
            ]),
            &[],
            &definitions,
            true
        ),
        None
    );
}

#[test]
fn document_identities_keep_their_own_rules_beside_the_key() {
    let definitions = columns("CREATE TABLE t (id integer PRIMARY KEY, v text)");
    let identity = |value| equal(column(uqa_sql::semantics::DOC_ID_COLUMN), int(value));
    assert_eq!(keyed(&identity(4), &[], &definitions, false), Some(vec![4]));
    // No row has a negative identity.
    assert_eq!(keyed(&identity(-4), &[], &definitions, false), Some(vec![]));
    let conflicting = ScalarExpr::And(vec![identity(4), equal(column("id"), int(5))]);
    assert_eq!(keyed(&conflicting, &[], &definitions, true), Some(vec![]));
    // A column named `_doc_id` keeps its own meaning.
    let own = columns("CREATE TABLE t (_doc_id integer, v text)");
    assert_eq!(keyed(&identity(4), &[], &own, true), None);
}

#[test]
fn an_aliased_key_is_named_as_the_filter_writes_it() {
    let definitions = columns("CREATE TABLE t (id integer PRIMARY KEY, v text)");
    let aliased = IdentityColumns::new(&definitions, true, |_| "renamed");
    assert_eq!(
        key_candidates(&equal(column("renamed"), int(3)), &[], aliased),
        Some(vec![3])
    );
    assert_eq!(
        key_candidates(&equal(column("id"), int(3)), &[], aliased),
        None
    );
}
