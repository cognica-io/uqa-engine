//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inline relation capture uses the shared scoped binder and admitted catalog names.

use super::RoutineInliningContext;
use crate::{catalog::resolution::RelationLookupMode, plan::MutableQueryPlan, SQLError};
use uqa_core::memory::ProductionControl;

impl RoutineInliningContext<'_> {
    pub(super) fn bind_inline_relations_owned(
        &self,
        query: MutableQueryPlan,
        control: &ProductionControl<'_>,
    ) -> Result<MutableQueryPlan, SQLError> {
        let namespace = self.catalog.stored_query_namespace_with_control(control)?;
        let mode = if query.relations_bound {
            RelationLookupMode::Bound
        } else {
            RelationLookupMode::Dynamic
        };
        crate::binding::view_dependencies::bind_query_relation_targets_owned(
            query,
            &|name| namespace.transition_relations.contains(name),
            false,
            control,
            &mut |name, _, control| {
                crate::binding::stored_relations::canonical_catalog_relation_with_control(
                    self.relations,
                    name,
                    mode,
                    false,
                    "function",
                    control,
                )
                .map(|(name, _)| name)
            },
        )
    }
}
