//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::Value;
use uqa_planner::{AccessPathPlan, JoinExecutionStrategy};
use uqa_sql::ast::{BinaryOp, JoinKind};

fn equality(left: &str, right: &str) -> ScalarExpr {
    ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(ScalarExpr::Column(left.into())),
        rhs: Box::new(ScalarExpr::Column(right.into())),
    }
}

fn qualified_literal_equality(qualifier: &str, column: &str, value: &str) -> ScalarExpr {
    ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(ScalarExpr::qualified_column(qualifier, column)),
        rhs: Box::new(ScalarExpr::Literal(Value::Str(value.into()))),
    }
}

fn joined_source(kind: JoinKind, on: ScalarExpr) -> SourcePlan {
    SourcePlan::Join {
        left: Box::new(SourcePlan::Table {
            name: "left_table".into(),
            qualifier: "left_table".into(),
            alias: Some("l".into()),
            include_descendants: true,
        }),
        right: Box::new(SourcePlan::Table {
            name: "right_table".into(),
            qualifier: "right_table".into(),
            alias: Some("r".into()),
            include_descendants: true,
        }),
        kind,
        on: Some(on),
        using: None,
        natural: false,
        alias: None,
        column_aliases: Vec::new(),
        lateral: false,
        strategy: JoinExecutionStrategy::Hash,
    }
}

fn query_block(filter: ScalarExpr, from: SourcePlan) -> QueryBlockPlan {
    QueryBlockPlan {
        projections: Vec::new(),
        from: Some(from),
        r#where: Some(filter),
        compute: ComputePlan::Project,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        group_distinct: false,
        having: None,
        order_by: Vec::new(),
        limit: None,
        with_ties: false,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        subqueries: Vec::new(),
        access: AccessPathPlan::Row,
        locking: Vec::new(),
    }
}

#[test]
fn unique_unqualified_owner_enables_safe_filter_pushdown() {
    let engine = Engine::new();
    let owners = BTreeMap::from([
        ("p_name".into(), Some("part".into())),
        ("shared".into(), None),
    ]);
    let qualifiers = BTreeSet::from(["part".into(), "lineitem".into()]);
    let predicate = ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(ScalarExpr::Column("p_name".into())),
        rhs: Box::new(ScalarExpr::Literal(Value::Str("green".into()))),
    };

    let (qualifier, pushed) =
        qualifier_filter_for_part(&engine, &predicate, &qualifiers, None, &owners, &[]).unwrap();
    assert_eq!(qualifier, "part");
    let ScalarExpr::Binary { lhs, .. } = pushed else {
        panic!("pushdown changed the predicate shape");
    };
    assert!(matches!(
        lhs.as_ref(),
        ScalarExpr::QualifiedColumn { qualifier, column, .. }
            if qualifier == "part" && column == "p_name"
    ));

    let ambiguous = ScalarExpr::Column("shared".into());
    assert!(
        qualifier_filter_for_part(&engine, &ambiguous, &qualifiers, None, &owners, &[]).is_none()
    );
}

#[test]
fn disjunction_derives_a_necessary_filter_for_every_complete_source_projection() {
    let engine = Engine::new();
    let qualifiers = BTreeSet::from(["n1".into(), "n2".into()]);
    let predicate = ScalarExpr::Or(vec![
        ScalarExpr::And(vec![
            qualified_literal_equality("n1", "name", "FRANCE"),
            qualified_literal_equality("n2", "name", "GERMANY"),
        ]),
        ScalarExpr::And(vec![
            qualified_literal_equality("n1", "name", "GERMANY"),
            qualified_literal_equality("n2", "name", "FRANCE"),
        ]),
    ]);

    let derived = derived_disjunctive_qualifier_filters(
        &engine,
        &predicate,
        &qualifiers,
        None,
        &BTreeMap::new(),
        &[],
    );
    assert_eq!(derived.len(), 2);
    for (qualifier, predicate) in derived {
        let ScalarExpr::Or(disjuncts) = predicate else {
            panic!("expected a projected disjunction")
        };
        assert_eq!(disjuncts.len(), 2);
        assert!(disjuncts
            .iter()
            .all(|part| expr_qualifiers(part) == BTreeSet::from([qualifier.clone()])));
    }
}

#[test]
fn disjunction_does_not_push_a_projection_missing_from_any_branch() {
    let engine = Engine::new();
    let qualifiers = BTreeSet::from(["n1".into(), "n2".into()]);
    let predicate = ScalarExpr::Or(vec![
        qualified_literal_equality("n1", "name", "FRANCE"),
        qualified_literal_equality("n2", "name", "GERMANY"),
    ]);

    assert!(derived_disjunctive_qualifier_filters(
        &engine,
        &predicate,
        &qualifiers,
        None,
        &BTreeMap::new(),
        &[],
    )
    .is_empty());
}

#[test]
fn inner_join_guarantee_elides_duplicate_where_conjunct() {
    let engine = Engine::new();
    let ctes = CteScope::new_for_current_routine(&engine);
    let join_equality = equality("l.key", "r.key");
    let residual = ScalarExpr::Literal(Value::Bool(true));
    let from = joined_source(JoinKind::Inner, join_equality.clone());
    let block = query_block(
        ScalarExpr::And(vec![join_equality, residual.clone()]),
        from.clone(),
    );

    assert_eq!(
        final_filter_after_qualifier_pushdown(&engine, &block, &from, None, &ctes).unwrap(),
        Some(residual)
    );
}

#[test]
fn outer_join_keeps_duplicate_where_conjunct() {
    let engine = Engine::new();
    let ctes = CteScope::new_for_current_routine(&engine);
    let join_equality = equality("l.key", "r.key");
    let filter = ScalarExpr::And(vec![
        join_equality.clone(),
        ScalarExpr::Literal(Value::Bool(true)),
    ]);
    let from = joined_source(JoinKind::Left, join_equality);
    let block = query_block(filter.clone(), from.clone());

    assert_eq!(
        final_filter_after_qualifier_pushdown(&engine, &block, &from, None, &ctes).unwrap(),
        Some(filter)
    );
}

#[test]
fn outer_join_marks_only_null_extended_qualifiers_as_unsafe_for_pushdown() {
    let join_equality = equality("l.key", "r.key");
    assert_eq!(
        outer_join_nullable_qualifiers(&joined_source(JoinKind::Left, join_equality.clone())),
        BTreeSet::from(["r".into()])
    );
    assert_eq!(
        outer_join_nullable_qualifiers(&joined_source(JoinKind::Right, join_equality.clone())),
        BTreeSet::from(["l".into()])
    );
    assert_eq!(
        outer_join_nullable_qualifiers(&joined_source(JoinKind::Full, join_equality.clone())),
        BTreeSet::from(["l".into(), "r".into()])
    );
    assert!(
        outer_join_nullable_qualifiers(&joined_source(JoinKind::Inner, join_equality)).is_empty()
    );

    let nested_outer = joined_source(JoinKind::Left, equality("l.key", "r.key"));
    let nested_alias = SourcePlan::Join {
        left: Box::new(nested_outer),
        right: Box::new(SourcePlan::Table {
            name: "marker_table".into(),
            qualifier: "marker_table".into(),
            alias: Some("marker".into()),
            include_descendants: true,
        }),
        kind: JoinKind::Cross,
        on: None,
        using: None,
        natural: false,
        alias: Some("joined".into()),
        column_aliases: Vec::new(),
        lateral: false,
        strategy: JoinExecutionStrategy::Auto,
    };
    assert_eq!(
        outer_join_nullable_qualifiers(&nested_alias),
        BTreeSet::from(["joined".into()]),
        "an alias around an inner join must retain a nested outer join's nullable output"
    );

    let mut aliased_inner = joined_source(JoinKind::Inner, equality("l.key", "r.key"));
    let SourcePlan::Join { alias, .. } = &mut aliased_inner else {
        unreachable!()
    };
    *alias = Some("joined".into());
    assert!(outer_join_nullable_qualifiers(&aliased_inner).is_empty());
}
