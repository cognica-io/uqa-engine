//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Window specifications as SQL: a referenced window, `PARTITION BY`, `ORDER BY` and the frame clause.

use super::{expr_list, expr_sql, ident, order_by_sql};
use crate::ast::{FrameBound, FrameMode, WindowReferenceKind, WindowSpec};

pub(super) fn window_sql(spec: &WindowSpec) -> String {
    if let Some(reference) = &spec.reference {
        if reference.kind == WindowReferenceKind::Direct
            && spec.partition_by.is_empty()
            && spec.order_by.is_empty()
            && spec.frame.is_none()
        {
            return ident(&reference.name);
        }
    }
    let mut parts = Vec::new();
    if let Some(reference) = &spec.reference {
        parts.push(ident(&reference.name));
    }
    if !spec.partition_by.is_empty() {
        parts.push(format!("PARTITION BY {}", expr_list(&spec.partition_by)));
    }
    if !spec.order_by.is_empty() {
        parts.push(format!("ORDER BY {}", order_by_sql(&spec.order_by)));
    }
    if let Some(frame) = &spec.frame {
        parts.push(frame_clause_sql(
            frame.mode,
            &frame_bound_sql(&frame.start),
            &frame_bound_sql(&frame.end),
            frame.between,
            frame.exclusion,
        ));
    }
    format!("({})", parts.join(" "))
}

/// A frame clause as `get_rule_windowspec` spells it: `BETWEEN` only when the frame was written with it, and the exclusion last.
#[must_use]
pub fn frame_clause_sql(
    mode: FrameMode,
    start: &str,
    end: &str,
    between: bool,
    exclusion: crate::ast::FrameExclusion,
) -> String {
    let mode = match mode {
        FrameMode::Rows => "ROWS",
        FrameMode::Range => "RANGE",
        FrameMode::Groups => "GROUPS",
    };
    let mut clause = if between {
        format!("{mode} BETWEEN {start} AND {end}")
    } else {
        format!("{mode} {start}")
    };
    if let Some(exclusion) = exclusion.sql() {
        clause.push(' ');
        clause.push_str(exclusion);
    }
    clause
}

fn frame_bound_sql(bound: &FrameBound) -> String {
    match bound {
        FrameBound::UnboundedPreceding => "UNBOUNDED PRECEDING".into(),
        FrameBound::UnboundedFollowing => "UNBOUNDED FOLLOWING".into(),
        FrameBound::CurrentRow => "CURRENT ROW".into(),
        FrameBound::Preceding(expression) => format!("{} PRECEDING", expr_sql(expression)),
        FrameBound::Following(expression) => format!("{} FOLLOWING", expr_sql(expression)),
    }
}
