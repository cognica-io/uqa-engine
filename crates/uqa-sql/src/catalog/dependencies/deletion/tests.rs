//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{
    Dependency, DependencyGraph, DependencyKind, ObjectAddress, ATTRIBUTE_DEFAULT_CLASS,
    CONSTRAINT_CLASS, PROCEDURE_CLASS, RELATION_CLASS, REWRITE_CLASS, TYPE_CLASS,
};
use super::*;
use std::collections::BTreeMap;

/// The objects of `tests/parity/pg18/type_lifecycle_oracle`, with OIDs in creation order: an enum `feeling`, a domain `posint2` over integer, a domain `good_mood` over the enum, a table `lt`, two indexes, a view `lv` and five routines.
struct Fixture {
    graph: DependencyGraph,
    names: BTreeMap<ObjectAddress, String>,
}

const FEELING_ARRAY: u32 = 100;
const FEELING: u32 = 101;
const POSINT_ARRAY: u32 = 102;
const POSINT: u32 = 103;
const POSINT_CHECK: u32 = 104;
const GOOD_MOOD_ARRAY: u32 = 105;
const GOOD_MOOD: u32 = 106;
const GOOD_MOOD_CHECK: u32 = 107;
const TABLE: u32 = 110;
const TABLE_ARRAY: u32 = 111;
const TABLE_ROW_TYPE: u32 = 112;
const DEFAULT_M: u32 = 113;
const GENERATED_GEN: u32 = 114;
const CHECK_C: u32 = 115;
const KEY_NOT_NULL: u32 = 109;
const KEY_INDEX: u32 = 116;
const KEY: u32 = 117;
const PARTIAL_INDEX: u32 = 118;
const CAST_INDEX: u32 = 119;
const VIEW: u32 = 120;
const VIEW_ARRAY: u32 = 121;
const VIEW_ROW_TYPE: u32 = 122;
const VIEW_RULE: u32 = 123;
const LT_NEXT: u32 = 124;
const LT_DOUBLE: u32 = 125;
const LT_ATOMIC: u32 = 126;
const LT_PLPGSQL: u32 = 127;
const LT_FRESH: u32 = 128;

fn object(class_id: u32, object_id: u32) -> ObjectAddress {
    ObjectAddress::whole(class_id, object_id)
}

fn ty(oid: u32) -> ObjectAddress {
    object(TYPE_CLASS, oid)
}

fn column(attnum: i32) -> ObjectAddress {
    ObjectAddress::column(TABLE, attnum)
}

fn view_column(attnum: i32) -> ObjectAddress {
    ObjectAddress::column(VIEW, attnum)
}

fn fixture() -> Fixture {
    Fixture {
        graph: DependencyGraph::new(fixture_edges().into_iter().map(
            |(dependent, referenced, kind)| Dependency {
                dependent,
                referenced,
                kind,
            },
        )),
        names: fixture_names(),
    }
}

fn fixture_edges() -> Vec<(ObjectAddress, ObjectAddress, DependencyKind)> {
    use DependencyKind::{Auto, Internal, Normal};
    vec![
        (ty(FEELING_ARRAY), ty(FEELING), Internal),
        (ty(POSINT_ARRAY), ty(POSINT), Internal),
        (object(CONSTRAINT_CLASS, POSINT_CHECK), ty(POSINT), Auto),
        (ty(GOOD_MOOD_ARRAY), ty(GOOD_MOOD), Internal),
        (ty(GOOD_MOOD), ty(FEELING), Normal),
        (
            object(CONSTRAINT_CLASS, GOOD_MOOD_CHECK),
            ty(GOOD_MOOD),
            Auto,
        ),
        (
            object(CONSTRAINT_CLASS, GOOD_MOOD_CHECK),
            ty(FEELING),
            Normal,
        ),
        (column(2), ty(FEELING), Normal),
        (column(3), ty(FEELING_ARRAY), Normal),
        (column(4), ty(POSINT), Normal),
        (column(5), ty(GOOD_MOOD), Normal),
        (column(6), ty(GOOD_MOOD_ARRAY), Normal),
        (column(7), ty(FEELING), Normal),
        (ty(TABLE_ROW_TYPE), object(RELATION_CLASS, TABLE), Internal),
        (ty(TABLE_ARRAY), ty(TABLE_ROW_TYPE), Internal),
        (object(ATTRIBUTE_DEFAULT_CLASS, DEFAULT_M), column(2), Auto),
        (
            object(ATTRIBUTE_DEFAULT_CLASS, DEFAULT_M),
            ty(FEELING),
            Normal,
        ),
        (
            object(ATTRIBUTE_DEFAULT_CLASS, GENERATED_GEN),
            column(8),
            Internal,
        ),
        (
            object(ATTRIBUTE_DEFAULT_CLASS, GENERATED_GEN),
            column(2),
            Normal,
        ),
        (
            object(ATTRIBUTE_DEFAULT_CLASS, GENERATED_GEN),
            ty(FEELING),
            Normal,
        ),
        (object(CONSTRAINT_CLASS, CHECK_C), column(7), Auto),
        // The expression's column reference records a second, normal dependency.
        (object(CONSTRAINT_CLASS, CHECK_C), column(7), Normal),
        (object(CONSTRAINT_CLASS, KEY_NOT_NULL), column(1), Auto),
        (object(CONSTRAINT_CLASS, CHECK_C), ty(FEELING), Normal),
        (
            object(RELATION_CLASS, KEY_INDEX),
            object(CONSTRAINT_CLASS, KEY),
            Internal,
        ),
        (object(CONSTRAINT_CLASS, KEY), column(1), Auto),
        (object(RELATION_CLASS, PARTIAL_INDEX), column(1), Auto),
        (object(RELATION_CLASS, PARTIAL_INDEX), column(2), Auto),
        (object(RELATION_CLASS, PARTIAL_INDEX), ty(FEELING), Normal),
        (object(RELATION_CLASS, CAST_INDEX), column(4), Auto),
        (
            object(RELATION_CLASS, CAST_INDEX),
            object(RELATION_CLASS, TABLE),
            Auto,
        ),
        (
            object(REWRITE_CLASS, VIEW_RULE),
            object(RELATION_CLASS, VIEW),
            Internal,
        ),
        (ty(VIEW_ROW_TYPE), object(RELATION_CLASS, VIEW), Internal),
        (ty(VIEW_ARRAY), ty(VIEW_ROW_TYPE), Internal),
        (view_column(2), ty(FEELING), Normal),
        (view_column(4), ty(FEELING), Normal),
        (view_column(6), ty(POSINT), Normal),
        (object(REWRITE_CLASS, VIEW_RULE), column(1), Normal),
        (object(REWRITE_CLASS, VIEW_RULE), column(2), Normal),
        (object(REWRITE_CLASS, VIEW_RULE), column(4), Normal),
        (object(REWRITE_CLASS, VIEW_RULE), ty(FEELING), Normal),
        (object(REWRITE_CLASS, VIEW_RULE), ty(POSINT), Normal),
        (object(PROCEDURE_CLASS, LT_NEXT), ty(FEELING), Normal),
        (object(PROCEDURE_CLASS, LT_DOUBLE), ty(POSINT), Normal),
        (object(PROCEDURE_CLASS, LT_ATOMIC), ty(FEELING), Normal),
        (object(PROCEDURE_CLASS, LT_PLPGSQL), ty(FEELING), Normal),
        (object(PROCEDURE_CLASS, LT_FRESH), ty(FEELING), Normal),
    ]
}

fn fixture_names() -> BTreeMap<ObjectAddress, String> {
    let mut names = BTreeMap::new();
    for (address, name) in [
        (ty(FEELING), "type other.feeling"),
        (ty(FEELING_ARRAY), "type other.feeling[]"),
        (ty(POSINT), "type other.posint2"),
        (ty(POSINT_ARRAY), "type other.posint2[]"),
        (ty(GOOD_MOOD), "type good_mood"),
        (ty(GOOD_MOOD_ARRAY), "type good_mood[]"),
        (
            object(CONSTRAINT_CLASS, POSINT_CHECK),
            "constraint posint_check",
        ),
        (
            object(CONSTRAINT_CLASS, GOOD_MOOD_CHECK),
            "constraint good_mood_check",
        ),
        (object(RELATION_CLASS, TABLE), "table lt"),
        (ty(TABLE_ROW_TYPE), "type lt"),
        (ty(TABLE_ARRAY), "type lt[]"),
        (
            object(ATTRIBUTE_DEFAULT_CLASS, DEFAULT_M),
            "default value for column m of table lt",
        ),
        (
            object(ATTRIBUTE_DEFAULT_CLASS, GENERATED_GEN),
            "default value for column gen of table lt",
        ),
        (
            object(CONSTRAINT_CLASS, CHECK_C),
            "constraint lt_c_check on table lt",
        ),
        (
            object(CONSTRAINT_CLASS, KEY_NOT_NULL),
            "constraint lt_id_not_null on table lt",
        ),
        (object(RELATION_CLASS, KEY_INDEX), "index lt_pkey"),
        (
            object(CONSTRAINT_CLASS, KEY),
            "constraint lt_pkey on table lt",
        ),
        (
            object(RELATION_CLASS, PARTIAL_INDEX),
            "index lt_partial_idx",
        ),
        (object(RELATION_CLASS, CAST_INDEX), "index lt_cast_idx"),
        (object(RELATION_CLASS, VIEW), "view lv"),
        (ty(VIEW_ROW_TYPE), "type lv"),
        (ty(VIEW_ARRAY), "type lv[]"),
        (object(REWRITE_CLASS, VIEW_RULE), "rule _RETURN on view lv"),
        (
            object(PROCEDURE_CLASS, LT_NEXT),
            "function lt_next(other.feeling)",
        ),
        (
            object(PROCEDURE_CLASS, LT_DOUBLE),
            "function lt_double(other.posint2)",
        ),
        (
            object(PROCEDURE_CLASS, LT_ATOMIC),
            "function lt_atomic(other.feeling)",
        ),
        (
            object(PROCEDURE_CLASS, LT_PLPGSQL),
            "function lt_plpgsql(other.feeling)",
        ),
        (
            object(PROCEDURE_CLASS, LT_FRESH),
            "function lt_fresh(other.feeling)",
        ),
    ] {
        names.insert(address, name.to_owned());
    }
    for (attnum, name) in [
        (1, "id"),
        (2, "m"),
        (3, "ms"),
        (4, "p"),
        (5, "g"),
        (6, "gs"),
        (7, "c"),
        (8, "gen"),
    ] {
        names.insert(column(attnum), format!("column {name} of table lt"));
    }
    for (attnum, name) in [(2, "m"), (4, "top"), (6, "np")] {
        names.insert(view_column(attnum), format!("column {name} of view lv"));
    }
    names
}

impl Fixture {
    fn describe(&self) -> impl Fn(ObjectAddress) -> Result<Option<String>, SQLError> + '_ {
        |address| Ok(self.names.get(&address).cloned())
    }

    fn restrict_detail(&self, original: ObjectAddress) -> (String, Option<String>) {
        let describe = self.describe();
        let targets = DeletionTargets::collect(&self.graph, &[original], &describe).unwrap();
        match targets.report(false, Some(original), &describe) {
            Err(SQLError::Diagnostic {
                sqlstate,
                message,
                detail,
                hint,
            }) => {
                assert_eq!(sqlstate, "2BP01");
                assert_eq!(
                    hint.as_deref(),
                    Some("Use DROP ... CASCADE to drop the dependent objects too.")
                );
                (message, detail)
            }
            other => panic!("expected a dependency error, got {other:?}"),
        }
    }
}

#[test]
fn dependents_are_reported_in_postgresql_order() {
    let fixture = fixture();
    let (message, detail) = fixture.restrict_detail(object(TYPE_CLASS, FEELING));
    assert_eq!(
        message,
        "cannot drop type other.feeling because other objects depend on it"
    );
    assert_eq!(
        detail.as_deref(),
        Some(
            "column ms of table lt depends on type other.feeling[]\n\
             type good_mood depends on type other.feeling\n\
             column gs of table lt depends on type good_mood[]\n\
             column g of table lt depends on type good_mood\n\
             column c of table lt depends on type other.feeling\n\
             column m of table lt depends on type other.feeling\n\
             column gen of table lt depends on type other.feeling\n\
             view lv depends on type other.feeling\n\
             function lt_next(other.feeling) depends on type other.feeling\n\
             function lt_atomic(other.feeling) depends on type other.feeling\n\
             function lt_plpgsql(other.feeling) depends on type other.feeling\n\
             function lt_fresh(other.feeling) depends on type other.feeling"
        )
    );
    let (_, detail) = fixture.restrict_detail(object(TYPE_CLASS, POSINT));
    assert_eq!(
        detail.as_deref(),
        Some(
            "column p of table lt depends on type other.posint2\n\
             view lv depends on type other.posint2\n\
             function lt_double(other.posint2) depends on type other.posint2"
        )
    );
    let (_, detail) = fixture.restrict_detail(object(TYPE_CLASS, GOOD_MOOD));
    assert_eq!(
        detail.as_deref(),
        Some(
            "column gs of table lt depends on type good_mood[]\n\
             column g of table lt depends on type good_mood"
        )
    );
}

#[test]
fn an_internal_object_names_its_owner() {
    let fixture = fixture();
    let describe = fixture.describe();
    let error = DeletionTargets::collect(
        &fixture.graph,
        &[object(TYPE_CLASS, FEELING_ARRAY)],
        &describe,
    )
    .unwrap_err();
    let SQLError::Diagnostic { message, hint, .. } = error else {
        panic!("expected a dependency error, got {error:?}")
    };
    assert_eq!(
        message,
        "cannot drop type other.feeling[] because type other.feeling requires it"
    );
    assert_eq!(
        hint.as_deref(),
        Some("You can drop type other.feeling instead.")
    );
    // Naming the owner as well deletes the internal object with it.
    let targets = DeletionTargets::collect(
        &fixture.graph,
        &[
            object(TYPE_CLASS, FEELING_ARRAY),
            object(TYPE_CLASS, GOOD_MOOD),
        ],
        &describe,
    );
    assert!(targets.is_err(), "the array's owner is not named");
    let targets = DeletionTargets::collect(
        &fixture.graph,
        &[
            object(TYPE_CLASS, FEELING_ARRAY),
            object(TYPE_CLASS, FEELING),
        ],
        &describe,
    )
    .unwrap();
    assert!(targets
        .targets()
        .iter()
        .any(|target| target.object == object(TYPE_CLASS, FEELING_ARRAY)));
}

#[test]
fn cascades_list_the_same_objects_and_delete_dependents_first() {
    let fixture = fixture();
    let describe = fixture.describe();
    let original = object(TYPE_CLASS, POSINT);
    let targets = DeletionTargets::collect(&fixture.graph, &[original], &describe).unwrap();
    assert_eq!(
        targets.report(true, Some(original), &describe).unwrap(),
        Some(CascadeNotice {
            message: "drop cascades to 3 other objects".into(),
            detail: Some(
                "drop cascades to column p of table lt\n\
                 drop cascades to view lv\n\
                 drop cascades to function lt_double(other.posint2)"
                    .into()
            ),
        })
    );
    let order = targets
        .targets()
        .iter()
        .map(|target| target.object)
        .collect::<Vec<_>>();
    let position = |address| order.iter().position(|object| *object == address).unwrap();
    // Every dependent precedes what it depends on, and the named object comes last.
    assert!(position(ObjectAddress::column(TABLE, 4)) < position(original));
    assert!(position(object(REWRITE_CLASS, VIEW_RULE)) < position(object(RELATION_CLASS, VIEW)));
    assert_eq!(order.last(), Some(&original));
    // A single dependent is the whole notice.
    let original = object(TYPE_CLASS, GOOD_MOOD);
    let single = DependencyGraph::new(
        fixture
            .graph
            .edges()
            .iter()
            .copied()
            .filter(|edge| edge.dependent != ObjectAddress::column(TABLE, 6)),
    );
    let targets = DeletionTargets::collect(&single, &[original], &describe).unwrap();
    assert_eq!(
        targets.report(true, Some(original), &describe).unwrap(),
        Some(CascadeNotice {
            message: "drop cascades to column g of table lt".into(),
            detail: None,
        })
    );
}

#[test]
fn several_named_objects_are_reported_together() {
    let fixture = fixture();
    let describe = fixture.describe();
    let targets = DeletionTargets::collect(
        &fixture.graph,
        &[object(TYPE_CLASS, POSINT), object(TYPE_CLASS, GOOD_MOOD)],
        &describe,
    )
    .unwrap();
    let error = targets.report(false, None, &describe).unwrap_err();
    assert_eq!(
        error.to_string(),
        "cannot drop desired object(s) because other objects depend on them"
    );
}
