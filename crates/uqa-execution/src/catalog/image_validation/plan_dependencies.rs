//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored queries borrow the selected catalog's relation namespace without recompilation.

use crate::catalog::{relation_names::RelationNameCatalog, CatalogReadView};
use uqa_core::memory::ProductionControl;
use uqa_sql::{
    binding::view_dependencies::visit_stored_plan_relation_references,
    catalog::resolution::bound_relation_identity_with_control,
    plan::PlanRef,
    routines::{CompiledFunctionBody, RoutineBody},
    SQLError,
};

/// Validate concrete relation references of stored views, bound SQL routine plans and event definitions. Deferred source bodies keep their existing lazy compilation contract. Graph-label lookup follows the same immutable catalog's graph owner. No source SQL, callback or query is executed.
pub fn validate(
    catalog: &CatalogReadView,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let snapshot = catalog.snapshot();
    let names = RelationNameCatalog::new(&snapshot.tables, &snapshot.definitions);
    let mut reference = |name: &str, _: bool| {
        control.check()?;
        let identity = bound_relation_identity_with_control(name, control)?;
        if names.kind_at(&identity).is_none()
            && catalog
                .graph_label_kind(&identity.schema, &identity.name)?
                .is_none()
        {
            return Err(SQLError::Internal("missing native plan relation".into()));
        }
        control.check()?;
        Ok(())
    };
    for view in snapshot.definitions.views.values() {
        visit_stored_plan_relation_references(
            PlanRef::Query(&view.query),
            control,
            &mut reference,
        )?;
    }
    for routine in snapshot.definitions.sql_user_functions.values().flatten() {
        control.check()?;
        if let RoutineBody::Bound(body) = &routine.body {
            let CompiledFunctionBody::SQL(plans) = &***body else {
                return Err(SQLError::Internal(
                    "native catalog contains a session routine body".into(),
                ));
            };
            for plan in plans {
                visit_stored_plan_relation_references(plan.into(), control, &mut reference)?;
            }
        }
    }
    super::event_dependencies::validate(catalog, control)
}

#[cfg(test)]
mod tests;
