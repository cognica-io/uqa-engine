//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, Node, SQLError};
use crate::catalog::image::input::scalars::References;

pub(in super::super) fn observe(node: &Node, references: &mut References) {
    if let Node::WindowSpec(spec) = node {
        references.window = references.window.max(spec.definition);
    }
}

pub(in super::super) fn carries_enclosing_references(node: &Node) -> bool {
    !matches!(
        node,
        Node::Query(_)
            | Node::Statement(_)
            | Node::Set(_)
            | Node::Cte(_)
            | Node::Insert(_)
            | Node::Update(_)
            | Node::Delete(_)
            | Node::Merge(_)
    )
}

pub(in super::super) fn validate(
    node: &Node,
    references: &References,
    input: &Input<'_>,
) -> Result<(), SQLError> {
    let mut position = 0;
    loop {
        input.binary.cancellation.check()?;
        if validate_step(node, references, position)? {
            return Ok(());
        }
        position += 1;
    }
}

pub(in super::super) fn validate_step(
    node: &Node,
    references: &References,
    position: usize,
) -> Result<bool, SQLError> {
    let windows = match node {
        Node::Query(query) => {
            if let Some(window) = query.windows.get(position) {
                if window.spec.definition.is_some() {
                    return Err(invalid("stored canonical window address"));
                }
                if window.inherited.is_some_and(|parent| {
                    parent >= position || query.windows[parent].name.is_none()
                }) {
                    return Err(invalid("stored window inheritance address"));
                }
                return Ok(false);
            }
            query.windows.len()
        }
        Node::Statement(_)
        | Node::Set(_)
        | Node::Cte(_)
        | Node::Insert(_)
        | Node::Update(_)
        | Node::Delete(_)
        | Node::Merge(_)
        | Node::Routine(_)
        | Node::Rule(_) => 0,
        Node::MultipleTargets(targets) => {
            if targets.targets.len() != targets.source_positions.len() {
                return Err(invalid("stored multiple assignment positions"));
            }
            if let Some(position) = targets.source_positions.get(position) {
                if *position >= targets.source_width {
                    return Err(invalid("stored multiple assignment position"));
                }
                return Ok(false);
            }
            return Ok(true);
        }
        _ => return Ok(true),
    };
    if references.window.is_some_and(|index| index >= windows) {
        return Err(invalid("stored window arena address"));
    }
    Ok(true)
}
