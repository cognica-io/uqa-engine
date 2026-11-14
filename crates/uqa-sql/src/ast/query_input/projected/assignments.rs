//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    finish, frame, special::untagged, Input, JsonDiagnostic, JsonInputError, JsonPosition, Parts,
    Projected, Read, Result,
};
use crate::ast::{
    relation_lifecycle::AlterForeignTableStmtSerde, AlterForeignTableStmt, AssignmentTarget,
    AssignmentTargets, IndexKey,
};

impl Projected for AssignmentTarget {
    const BUFFERED: bool = true;
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            if input.value.as_str().is_some() {
                let column = input.read::<String>(work).await?;
                let mut parts = Parts::new(input.control);
                let value = Self {
                    column: parts.take(column),
                    indirection: Vec::new(),
                };
                return finish(parts, value);
            }
            let value = untagged(
                projected_fields!(
                    input,
                    work,
                    AssignmentTarget {
                        column: required,
                        indirection: required
                    }
                )?
                .await,
                "Target",
            );
            value
        })
    }
}

impl Projected for AssignmentTargets {
    const BUFFERED: bool = true;
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            let mut parts = Parts::new(input.control);
            let value = match input.read::<AssignmentTarget>(work).await {
                Ok(single) => Self::Single(parts.take(single)),
                Err(JsonInputError::Diagnostic(_)) => Self::Multiple(
                    parts.take(untagged(input.read(work).await, "AssignmentTargets")?),
                ),
                Err(error) => return Err(error),
            };
            finish(parts, value)
        })
    }
}

impl Projected for IndexKey {
    const BUFFERED: bool = true;
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            let mut parts = Parts::new(input.control);
            let value = if input.value.as_str().is_some() {
                Self::Column(parts.take(input.read::<String>(work).await?))
            } else {
                Self::Expression(parts.take(untagged(input.read(work).await, "IndexKey")?))
            };
            finish(parts, value)
        })
    }
}

impl Projected for AlterForeignTableStmt {
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            let mut parts = Parts::new(input.control);
            let stored: AlterForeignTableStmtSerde = parts.take(input.read(work).await?);
            let value = Self::try_from(stored)
                .map_err(|message| JsonDiagnostic::message(message, JsonPosition::unlocated()))?;
            finish(parts, value)
        })
    }
}
