//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Automatic-view expression retargeting, check propagation, and RETURNING rewriting.

use super::{SQLError, TriggerEvent, TriggerTiming, ViewRewriteContext};
pub(super) mod owned;

pub(super) fn instead_of_trigger_definition(
    services: ViewRewriteContext<'_>,
    view: &str,
    event: TriggerEvent,
) -> Result<bool, SQLError> {
    services
        .catalog
        .has_trigger_definition(view, TriggerTiming::InsteadOf, event, true)
}

#[cfg(test)]
mod tests {
    use super::owned;
    use crate::{plan::OwnedProjection, production::Buffer, RowSchema, ScalarExpr};
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };

    #[test]
    fn empty_returning_source_stars_keep_relation_scope() {
        let source = RowSchema::with_qualified_types("q", Vec::new(), Vec::new());
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let star = |qualifier: &str| OwnedProjection {
            expression: ScalarExpr::QualifiedStar(qualifier.into())
                .copy_for_rewrite(&control)
                .unwrap(),
            alias: None,
        };
        let mut output = Buffer::new(&control);
        owned::expand_projection(&mut output, star("q"), &source, 2, &control).unwrap();
        assert!(output.is_empty());
        let error = owned::expand_projection(&mut output, star("missing"), &source, 2, &control)
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P01"));
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}
