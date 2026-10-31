//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rule RETURNING image requirements, positional contracts, and action rewriting.
use crate::{
    ast::{RuleEvent, Statement},
    SQLError,
};
use std::collections::BTreeSet;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuleReturningRequest {
    capture: bool,
    images: RuleReturningImages,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RuleReturningImages(u8);

impl RuleReturningImages {
    const CURRENT: Self = Self(1 << 0);
    const OLD: Self = Self(1 << 1);
    const NEW: Self = Self(1 << 2);

    fn insert(&mut self, image: Self) {
        self.0 |= image.0;
    }

    const fn contains(self, image: Self) -> bool {
        self.0 & image.0 != 0
    }
}

mod inspection;

#[cfg(test)]
mod tests;

pub fn validate_rule_returning_provider_width(
    provider_width: usize,
    event_width: usize,
) -> Result<(), SQLError> {
    if provider_width < event_width {
        return Err(SQLError::Internal(format!(
            "could not find replacement targetlist entry for attno {}",
            provider_width + 1
        )));
    }
    if provider_width > event_width {
        return Err(SQLError::Internal(format!(
            "rewrite-rule RETURNING provider produced {provider_width} columns, expected {event_width}"
        )));
    }
    Ok(())
}

/// Describe the stored action in its event namespace, before unevaluated caller inputs replace OLD and NEW. Stored stars keep their creation-time width.
pub fn rule_action_returning_schema(
    sources: &dyn super::action_binding::RuleSourceCatalog,
    context: crate::semantics::returning::ReturningAnalysisContext<'_>,
    action: &Statement,
    event: RuleEvent,
    columns: &[crate::ast::ColumnDef],
) -> Result<crate::RowSchema, SQLError> {
    let action_columns = super::action_binding::rule_action_target_columns(sources, action)?;
    let mut resolver =
        crate::catalog::events::validation::RuleRowTypeResolver::for_action(columns, event);
    let bound =
        super::action_binding::bind_rule_action(sources, action, &action_columns, &mut resolver)?;
    crate::semantics::returning::dml_statement_returning_schema(context, bound)?
        .ok_or_else(|| SQLError::Internal("rule action lost its RETURNING descriptor".into()))
}

mod augmentation;
pub use augmentation::augment_rule_returning_action;
pub(super) use augmentation::produce_rule_returning;
