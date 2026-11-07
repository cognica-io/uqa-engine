//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inline destinations take every original payload before their enclosing box retires.

use crate::ast::{CompositeRowBinding, SelectStmt, WindowSpec};
use std::mem::take;

pub(super) fn row(value: &mut CompositeRowBinding) -> CompositeRowBinding {
    let CompositeRowBinding {
        ty,
        attributes,
        layout,
        argument_types,
    } = value;
    CompositeRowBinding {
        ty: take(ty),
        attributes: take(attributes),
        layout: *layout,
        argument_types: argument_types.take(),
    }
}

pub(super) fn window(value: &mut WindowSpec) -> WindowSpec {
    let WindowSpec {
        raw_definition,
        definition,
        reference,
        partition_by,
        order_by,
        frame,
    } = value;
    WindowSpec {
        raw_definition: raw_definition.take(),
        definition: *definition,
        reference: reference.take(),
        partition_by: take(partition_by),
        order_by: take(order_by),
        frame: frame.take(),
    }
}

pub(super) fn query(value: &mut SelectStmt) -> SelectStmt {
    let SelectStmt {
        windows,
        projections,
        values,
        from,
        r#where,
        group_by,
        grouping_sets,
        group_distinct,
        having,
        order_by,
        limit,
        with_ties,
        offset,
        with,
        set_op,
        distinct,
        distinct_on,
        locking,
    } = value;
    SelectStmt {
        windows: take(windows),
        projections: take(projections),
        values: take(values),
        from: from.take(),
        r#where: r#where.take(),
        group_by: take(group_by),
        grouping_sets: take(grouping_sets),
        group_distinct: *group_distinct,
        having: having.take(),
        order_by: take(order_by),
        limit: limit.take(),
        with_ties: *with_ties,
        offset: offset.take(),
        with: take(with),
        set_op: set_op.take(),
        distinct: *distinct,
        distinct_on: take(distinct_on),
        locking: take(locking),
    }
}
