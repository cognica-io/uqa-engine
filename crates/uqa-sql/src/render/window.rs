//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Window specifications as SQL: a referenced window, `PARTITION BY`, `ORDER BY` and the frame clause.

use super::{expr_list, ident, order_by_sql, render_expr};
use crate::ast::{FrameBound, FrameMode, WindowDefinition, WindowReferenceKind, WindowSpec};
use crate::SQLError;

pub(super) fn window_sql(
    spec: &WindowSpec,
    windows: &[WindowDefinition],
) -> Result<String, SQLError> {
    if let Some(slot) = spec.definition.filter(|_| !windows.is_empty()) {
        let definition = windows.get(slot).ok_or_else(|| {
            SQLError::Internal("window call has no query-local definition".into())
        })?;
        return definition
            .name
            .as_ref()
            .map_or_else(|| definition_sql(slot, windows), |name| Ok(ident(name)));
    }
    if let Some(reference) = &spec.reference {
        if reference.kind == WindowReferenceKind::Direct
            && spec.partition_by.is_empty()
            && spec.order_by.is_empty()
            && spec.frame.is_none()
        {
            return Ok(ident(&reference.name));
        }
    }
    specification_sql(
        spec,
        spec.reference
            .as_ref()
            .map(|reference| reference.name.as_str()),
    )
}

pub(super) fn window_clause_sql(windows: &[WindowDefinition]) -> Result<String, SQLError> {
    let declarations = windows
        .iter()
        .enumerate()
        .filter_map(|(slot, definition)| {
            definition.name.as_ref().map(|name| {
                Ok(format!(
                    "{} AS {}",
                    ident(name),
                    definition_sql(slot, windows)?
                ))
            })
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    Ok(if declarations.is_empty() {
        String::new()
    } else {
        format!(" WINDOW {}", declarations.join(", "))
    })
}

fn definition_sql(slot: usize, windows: &[WindowDefinition]) -> Result<String, SQLError> {
    let definition = &windows[slot];
    let inherited = definition
        .inherited
        .map(|parent| {
            windows
                .get(parent)
                .filter(|_| parent < slot)
                .and_then(|window| window.name.as_deref())
                .ok_or_else(|| {
                    SQLError::Internal("window inherits no preceding named definition".into())
                })
        })
        .transpose()?;
    specification_sql(&definition.spec, inherited)
}

fn specification_sql(spec: &WindowSpec, reference: Option<&str>) -> Result<String, SQLError> {
    let mut parts = Vec::new();
    if let Some(reference) = reference {
        parts.push(ident(reference));
    }
    if !spec.partition_by.is_empty() {
        parts.push(format!("PARTITION BY {}", expr_list(&spec.partition_by)?));
    }
    if !spec.order_by.is_empty() {
        parts.push(format!("ORDER BY {}", order_by_sql(&spec.order_by)?));
    }
    if let Some(frame) = &spec.frame {
        parts.push(frame_clause_sql(
            frame.mode,
            &frame_bound_sql(&frame.start)?,
            &frame_bound_sql(&frame.end)?,
            frame.between,
            frame.exclusion,
        ));
    }
    Ok(format!("({})", parts.join(" ")))
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

fn frame_bound_sql(bound: &FrameBound) -> Result<String, SQLError> {
    Ok(match bound {
        FrameBound::UnboundedPreceding => "UNBOUNDED PRECEDING".into(),
        FrameBound::UnboundedFollowing => "UNBOUNDED FOLLOWING".into(),
        FrameBound::CurrentRow => "CURRENT ROW".into(),
        FrameBound::Preceding(expression) => format!("{} PRECEDING", render_expr(expression)?),
        FrameBound::Following(expression) => format!("{} FOLLOWING", render_expr(expression)?),
    })
}
