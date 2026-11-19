//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored query and mutation field shapes preserve the existing Serde declarations.

use super::{number, unit, Input, InputError, JsonToken};
use crate::ast::{
    CteMaterialization, FrameExclusion, FrameMode, JoinKind, LockStrength, LockWait, NullsOrder,
    OverridingKind, SetOpKind, WindowReferenceKind,
};
use serde::de::DeserializeOwned;

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Expression,
    Select,
    Projection,
    Order,
    Window,
    WindowSpec,
    WindowReference,
    WindowFrame,
    FrameBound,
    WindowModifiers,
    From,
    Function,
    Relations,
    JoinUsing,
    Cte,
    Search,
    Cycle,
    Set,
    Lock,
    Insert,
    Update,
    Delete,
    Merge,
    MergeTarget,
    MergeWhen,
    Conflict,
    ConflictAction,
    Returning,
    Target,
    Targets,
    MultipleTargets,
    AssignmentStep,
}

type Scalar = for<'a, 'c> fn(&mut Input<'a, 'c>, JsonToken<'a>) -> Result<(), InputError>;

#[derive(Clone, Copy)]
pub(super) enum Shape {
    Text,
    Bool,
    Null,
    Bytes,
    Binding,
    ColumnType,
    Scalar(Scalar),
    Node(Kind),
    Optional(&'static Shape),
    Array(&'static Shape),
    Map(&'static Shape),
    Tuple(&'static [Shape]),
    Fields(&'static [Field]),
    Enum(&'static [(&'static str, Shape)]),
    Choice(&'static Shape, &'static Shape),
}

pub(super) struct Field {
    pub name: &'static str,
    pub shape: Shape,
    pub required: bool,
    pub sequence_required: bool,
}

impl Field {
    const fn r(name: &'static str, shape: Shape) -> Self {
        Self {
            name,
            shape,
            required: true,
            sequence_required: true,
        }
    }
    const fn o(name: &'static str, shape: Shape) -> Self {
        Self {
            name,
            shape,
            required: false,
            sequence_required: true,
        }
    }
    const fn d(name: &'static str, shape: Shape) -> Self {
        Self {
            name,
            shape,
            required: false,
            sequence_required: false,
        }
    }
}

macro_rules! object {
    ($name:ident { $($field:literal: $mode:ident($shape:expr)),* $(,)? }) => {
        pub(super) const $name: Shape = Shape::Fields(&[$(Field::$mode($field, $shape)),*]);
    };
}
macro_rules! array {
    ($value:expr) => {
        Shape::Array(&$value)
    };
}
macro_rules! optional {
    ($value:expr) => {
        Shape::Optional(&$value)
    };
}
macro_rules! node {
    ($name:ident) => {
        Shape::Node(Kind::$name)
    };
}
macro_rules! unit {
    ($name:ty) => {
        Shape::Scalar(unit_value::<$name>)
    };
}

const TEXT: Shape = Shape::Text;
const BOOL: Shape = Shape::Bool;
const EXPR: Shape = node!(Expression);
const U32: Shape = Shape::Scalar(integer::<u32>);
const U64: Shape = Shape::Scalar(integer::<u64>);
const USIZE: Shape = Shape::Scalar(integer::<usize>);
const I16: Shape = Shape::Scalar(integer::<i16>);
const ASSIGNMENTS: Shape = array!(Shape::Tuple(&[node!(Targets), EXPR]));

fn integer<T: crate::ast::stored_input::Integer>(
    _: &mut Input<'_, '_>,
    token: JsonToken<'_>,
) -> Result<(), InputError> {
    number::<T>(token).map(|_| ())
}

fn unit_value<'a, T: DeserializeOwned + Copy>(
    input: &mut Input<'a, '_>,
    token: JsonToken<'a>,
) -> Result<(), InputError> {
    unit::<T>(&input.unit_tag(token)?).map(|_| ())
}

object!(EXISTS { "body": r(node!(Select)), "negated": r(BOOL) });
object!(IN_SUBQUERY { "expr": r(EXPR), "body": r(node!(Select)), "negated": r(BOOL) });
object!(WINDOW_CALL {
    "name": r(TEXT), "args": r(array!(EXPR)), "spec": r(node!(WindowSpec)),
    "filter": d(optional!(EXPR)), "modifiers": d(node!(WindowModifiers)),
});
object!(SELECT {
    "windows": d(array!(node!(Window))), "projections": r(array!(node!(Projection))),
    "values": d(array!(array!(EXPR))), "from": o(optional!(node!(From))),
    "where": o(optional!(EXPR)), "group_by": r(array!(EXPR)),
    "grouping_sets": r(array!(array!(EXPR))), "group_distinct": d(BOOL),
    "having": o(optional!(EXPR)), "order_by": r(array!(node!(Order))),
    "limit": o(optional!(EXPR)), "with_ties": d(BOOL), "offset": o(optional!(EXPR)),
    "with": r(array!(node!(Cte))), "set_op": o(optional!(node!(Set))),
    "distinct": r(BOOL), "distinct_on": r(array!(EXPR)), "locking": d(array!(node!(Lock))),
});
object!(PROJECTION { "expr": r(EXPR), "alias": o(optional!(TEXT)) });
object!(ORDER { "expr": r(EXPR), "descending": r(BOOL), "nulls": o(optional!(unit!(NullsOrder))) });
object!(WINDOW { "name": o(optional!(TEXT)), "inherited": o(optional!(USIZE)), "spec": r(node!(WindowSpec)) });
object!(WINDOW_SPEC {
    "definition": d(optional!(USIZE)), "reference": d(optional!(node!(WindowReference))),
    "partition_by": r(array!(EXPR)), "order_by": r(array!(node!(Order))),
    "frame": o(optional!(node!(WindowFrame))),
});
object!(WINDOW_REFERENCE { "name": r(TEXT), "kind": r(unit!(WindowReferenceKind)) });
object!(WINDOW_FRAME {
    "mode": r(unit!(FrameMode)), "start": r(node!(FrameBound)), "end": r(node!(FrameBound)),
    "between": d(BOOL), "exclusion": d(unit!(FrameExclusion)),
});
const FRAME_BOUND: Shape = Shape::Enum(&[
    ("UnboundedPreceding", Shape::Null),
    ("UnboundedFollowing", Shape::Null),
    ("CurrentRow", Shape::Null),
    ("Preceding", EXPR),
    ("Following", EXPR),
]);
object!(WINDOW_MODIFIERS { "distinct": r(BOOL), "ordered": r(BOOL), "within_group": r(BOOL) });
object!(TABLE_SOURCE {
    "name": r(TEXT), "qualifier": r(TEXT), "alias": o(optional!(TEXT)),
    "column_aliases": d(array!(TEXT)), "bound_columns": d(optional!(array!(TEXT))),
    "include_descendants": d(BOOL),
});
object!(JOIN_SOURCE {
    "comma": d(BOOL), "left": r(node!(From)), "right": r(node!(From)), "kind": r(unit!(JoinKind)),
    "on": o(optional!(EXPR)), "using": d(optional!(node!(JoinUsing))), "natural": d(BOOL),
    "alias": d(optional!(TEXT)), "column_aliases": d(array!(TEXT)), "lateral": r(BOOL),
});
object!(VALUES_SOURCE {
    "lateral": d(BOOL), "rows": r(array!(array!(EXPR))), "alias": o(optional!(TEXT)),
    "column_aliases": r(array!(TEXT)), "internal_relation": d(optional!(U64)),
    "internal_column_types": d(array!(optional!(Shape::ColumnType))), "internal_column_collations": d(array!(optional!(U32))),
});
object!(FUNCTION_SOURCE {
    "name": r(TEXT), "binding": d(optional!(Shape::Binding)), "output_name": r(TEXT),
    "relations": d(optional!(node!(Relations))), "args": r(array!(EXPR)),
    "alias": o(optional!(TEXT)), "column_aliases": r(array!(TEXT)), "ordinality": d(BOOL),
    "column_types": d(array!(TEXT)),
});
object!(FUNCTION_GROUP {
    "functions": r(array!(node!(Function))), "alias": o(optional!(TEXT)),
    "column_aliases": r(array!(TEXT)), "ordinality": d(BOOL),
});
object!(SUBQUERY_SOURCE {
    "lateral": d(BOOL), "body": r(node!(Select)), "alias": o(optional!(TEXT)), "column_aliases": r(array!(TEXT)),
});
const FROM: Shape = Shape::Enum(&[
    ("Table", TABLE_SOURCE),
    ("Join", JOIN_SOURCE),
    ("Values", VALUES_SOURCE),
    ("Function", FUNCTION_SOURCE),
    ("FunctionGroup", FUNCTION_GROUP),
    ("Subquery", SUBQUERY_SOURCE),
]);
object!(FUNCTION {
    "name": r(TEXT), "binding": d(optional!(Shape::Binding)), "output_name": r(TEXT),
    "relations": d(optional!(node!(Relations))), "args": r(array!(EXPR)),
    "column_aliases": d(array!(TEXT)), "column_types": d(array!(TEXT)),
});
object!(RELATIONS { "left": r(TEXT), "right": r(TEXT) });
object!(JOIN_USING { "columns": r(array!(TEXT)), "alias": d(optional!(TEXT)) });
object!(CTE_FIELDS {
    "name": r(TEXT), "columns": r(array!(TEXT)), "recursive": r(BOOL),
    "materialization": d(unit!(CteMaterialization)), "search": d(optional!(node!(Search))),
    "cycle": d(optional!(node!(Cycle))),
});
object!(SEARCH { "columns": r(array!(TEXT)), "breadth_first": r(BOOL), "sequence_column": r(TEXT) });
object!(CYCLE {
    "columns": r(array!(TEXT)), "mark_column": r(TEXT), "mark_value": r(EXPR),
    "mark_default": r(EXPR), "path_column": r(TEXT),
});
object!(SET {
    "kind": r(unit!(SetOpKind)), "all": r(BOOL), "left": d(optional!(node!(Select))),
    "right": r(node!(Select)), "combined_order_by": r(array!(node!(Order))),
    "combined_limit": o(optional!(EXPR)), "combined_with_ties": d(BOOL), "combined_offset": o(optional!(EXPR)),
});
object!(LOCK { "strength": r(unit!(LockStrength)), "wait": r(unit!(LockWait)), "relations": r(array!(TEXT)) });
object!(INSERT {
    "table": r(TEXT), "target_relation_bound": d(BOOL), "target_qualifier": r(TEXT),
    "target_alias": d(optional!(TEXT)), "include_descendants": d(BOOL),
    "columns": r(array!(node!(Target))), "overriding": d(optional!(unit!(OverridingKind))),
    "with": r(array!(node!(Cte))), "rows": r(array!(array!(EXPR))),
    "select_source": o(optional!(node!(Select))), "on_conflict": o(optional!(node!(Conflict))),
    "returning": r(array!(node!(Projection))), "returning_aliases": r(node!(Returning)),
});
object!(UPDATE {
    "table": r(TEXT), "target_relation_bound": d(BOOL), "target_qualifier": r(TEXT),
    "target_alias": d(optional!(TEXT)), "include_descendants": d(BOOL), "assignments": r(ASSIGNMENTS),
    "where": o(optional!(EXPR)), "with": r(array!(node!(Cte))), "from": o(optional!(node!(From))),
    "returning": r(array!(node!(Projection))), "returning_aliases": r(node!(Returning)),
});
object!(DELETE {
    "table": r(TEXT), "target_relation_bound": d(BOOL), "target_qualifier": r(TEXT),
    "target_alias": d(optional!(TEXT)), "include_descendants": d(BOOL), "where": o(optional!(EXPR)),
    "with": r(array!(node!(Cte))), "using": o(optional!(node!(From))),
    "returning": r(array!(node!(Projection))), "returning_aliases": r(node!(Returning)),
});
object!(MERGE {
    "with": d(array!(node!(Cte))), "target": r(TEXT), "target_qualifier": r(TEXT),
    "target_alias": o(optional!(TEXT)), "target_column_bindings": d(Shape::Map(&node!(MergeTarget))),
    "include_descendants": d(BOOL), "source": r(node!(From)), "join_condition": r(EXPR),
    "when_clauses": r(array!(node!(MergeWhen))), "returning": r(array!(node!(Projection))),
    "returning_aliases": r(node!(Returning)),
});
object!(MERGE_TARGET {
    "object_id": r(Shape::Bytes), "attribute_number": d(optional!(I16)), "domain_dependencies": d(array!(U32)),
});
object!(MERGE_UPDATE { "condition": o(optional!(EXPR)), "assignments": r(ASSIGNMENTS) });
object!(MERGE_CONDITION { "condition": o(optional!(EXPR)) });
object!(MERGE_INSERT {
    "condition": o(optional!(EXPR)), "columns": r(array!(node!(Target))),
    "overriding": d(optional!(unit!(OverridingKind))), "values": r(array!(EXPR)),
});
const MERGE_WHEN: Shape = Shape::Enum(&[
    ("UpdateMatched", MERGE_UPDATE),
    ("DeleteMatched", MERGE_CONDITION),
    ("UpdateNotMatchedBySource", MERGE_UPDATE),
    ("DeleteNotMatchedBySource", MERGE_CONDITION),
    ("InsertNotMatched", MERGE_INSERT),
    ("NothingMatched", MERGE_CONDITION),
    ("NothingNotMatched", MERGE_CONDITION),
    ("NothingNotMatchedBySource", MERGE_CONDITION),
]);
object!(CONFLICT {
    "predicate": d(optional!(EXPR)), "constraint": d(optional!(TEXT)),
    "conflict_columns": r(array!(TEXT)), "expressions": d(array!(EXPR)), "action": r(node!(ConflictAction)),
});
object!(CONFLICT_UPDATE { "assignments": r(ASSIGNMENTS), "where": o(optional!(EXPR)) });
const CONFLICT_ACTION: Shape =
    Shape::Enum(&[("Nothing", Shape::Null), ("Update", CONFLICT_UPDATE)]);
object!(RETURNING { "old": r(TEXT), "new": r(TEXT), "old_explicit": d(BOOL), "new_explicit": d(BOOL) });
object!(PARTIAL_TARGET { "column": r(TEXT), "indirection": r(array!(node!(AssignmentStep))) });
const TARGET: Shape = Shape::Choice(&TEXT, &PARTIAL_TARGET);
const TARGETS: Shape = Shape::Choice(&node!(Target), &node!(MultipleTargets));
object!(MULTIPLE_TARGETS { "targets": r(array!(node!(Target))), "source_positions": r(array!(USIZE)), "source_width": r(USIZE) });
object!(SLICE { "lower": o(optional!(EXPR)), "upper": o(optional!(EXPR)) });
const ASSIGNMENT_STEP: Shape = Shape::Enum(&[("Field", TEXT), ("Index", EXPR), ("Slice", SLICE)]);

pub(super) fn node(kind: Kind) -> Shape {
    match kind {
        Kind::Select => SELECT,
        Kind::Projection => PROJECTION,
        Kind::Order => ORDER,
        Kind::Window => WINDOW,
        Kind::WindowSpec => WINDOW_SPEC,
        Kind::WindowReference => WINDOW_REFERENCE,
        Kind::WindowFrame => WINDOW_FRAME,
        Kind::FrameBound => FRAME_BOUND,
        Kind::WindowModifiers => WINDOW_MODIFIERS,
        Kind::From => FROM,
        Kind::Function => FUNCTION,
        Kind::Relations => RELATIONS,
        Kind::JoinUsing => JOIN_USING,
        Kind::Search => SEARCH,
        Kind::Cycle => CYCLE,
        Kind::Set => SET,
        Kind::Lock => LOCK,
        Kind::Insert => INSERT,
        Kind::Update => UPDATE,
        Kind::Delete => DELETE,
        Kind::Merge => MERGE,
        Kind::MergeTarget => MERGE_TARGET,
        Kind::MergeWhen => MERGE_WHEN,
        Kind::Conflict => CONFLICT,
        Kind::ConflictAction => CONFLICT_ACTION,
        Kind::Returning => RETURNING,
        Kind::Target => TARGET,
        Kind::Targets => TARGETS,
        Kind::MultipleTargets => MULTIPLE_TARGETS,
        Kind::AssignmentStep => ASSIGNMENT_STEP,
        Kind::Expression | Kind::Cte => unreachable!("custom stored syntax boundary"),
    }
}
