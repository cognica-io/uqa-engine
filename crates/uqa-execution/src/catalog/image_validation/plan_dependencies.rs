//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored queries borrow the selected catalog's relation namespace without recompilation.

use crate::catalog::{relation_names::RelationNameCatalog, CatalogReadView};
use uqa_core::memory::ProductionControl;
use uqa_sql::{
    ast::FunctionBody,
    binding::view_dependencies::visit_stored_plan_relation_references_with_work,
    catalog::{resolution::bound_relation_identity_with_control, CatalogWork},
    plan::{PlanRef, StoredStatementObserver, UnifiedPlan},
    routines::{CompiledFunctionBody, RoutineBody},
    SQLError,
};

/// Validate concrete relation references of stored views, bound SQL routine plans and event definitions. Deferred source bodies keep their existing lazy compilation contract. Graph-label lookup follows the same immutable catalog's graph owner. No source SQL, callback or query is executed.
pub fn validate(
    catalog: &CatalogReadView,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    crate::catalog::selection_work::inline(validate_with_work(catalog, control, &mut ()))
}

struct Relations<'a>(&'a CatalogReadView);

impl StoredStatementObserver for Relations<'_> {
    async fn plan<W: CatalogWork + Send>(
        &mut self,
        plan: PlanRef<'_>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        let catalog = self.0;
        let snapshot = catalog.snapshot();
        visit_stored_plan_relation_references_with_work(plan, control, work, &mut |name, _| {
            control.check()?;
            let identity = bound_relation_identity_with_control(name, control)?;
            if RelationNameCatalog::new(&snapshot.tables, &snapshot.definitions)
                .kind_at(&identity)
                .is_none()
                && catalog
                    .graph_label_kind(&identity.schema, &identity.name)?
                    .is_none()
            {
                return Err(SQLError::Internal("missing native plan relation".into()));
            }
            control.check()?;
            Ok(())
        })
        .await
    }
}

pub(super) async fn validate_with_work<W: CatalogWork + Send>(
    catalog: &CatalogReadView,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), SQLError> {
    let snapshot = catalog.snapshot();
    let mut relations = Relations(catalog);
    for view in snapshot.definitions.views.values() {
        relations
            .plan(PlanRef::Query(&view.query), control, work)
            .await?;
    }
    for overloads in snapshot.definitions.sql_user_functions.values() {
        work.checkpoint().await?;
        for routine in overloads {
            control.check()?;
            work.checkpoint().await?;
            if let FunctionBody::Statements(statements) = &routine.def.body {
                for statement in statements {
                    UnifiedPlan::visit_stored_statement_with_work(
                        statement,
                        control,
                        work,
                        &mut relations,
                    )
                    .await?;
                }
            }
            if let RoutineBody::Bound(body) = &routine.body {
                let CompiledFunctionBody::SQL(plans) = &***body else {
                    return Err(SQLError::Internal(
                        "native catalog contains a session routine body".into(),
                    ));
                };
                for plan in plans {
                    relations.plan(plan.into(), control, work).await?;
                }
            }
        }
    }
    super::event_dependencies::validate_with_work(catalog, control, work).await
}

#[cfg(test)]
mod tests;
