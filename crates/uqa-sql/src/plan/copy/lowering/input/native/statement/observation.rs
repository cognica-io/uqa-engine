//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{ProductionControl, Purpose, UnifiedPlan};
use crate::{ast::Statement, catalog::work::Work, plan::PlanRef, SQLError};
use std::future::Future;

/// A catalog predicate inspects the original native statement under its own scope and diagnostic order.
pub trait StoredStatementObserver {
    fn plan<W: Work + Send>(
        &mut self,
        plan: PlanRef<'_>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> impl Future<Output = Result<(), SQLError>> + Send;
}

impl UnifiedPlan {
    /// Lower a stored statement, visit its native fields, and retire its temporary tree and field leases on the caller's work schedule.
    pub async fn visit_stored_statement_with_work<W: Work + Send>(
        statement: &Statement,
        control: &ProductionControl<'_>,
        work: &mut W,
        observer: &mut (impl StoredStatementObserver + Send),
    ) -> Result<(), SQLError> {
        if control.budget().is_none() {
            let plan = Self::lower_borrowed_for_rewrite(statement, &|_: &str| false, control)?;
            return observer.plan(plan.as_plan_ref(), control, work).await;
        }
        let plan = Self::lower_retained_statement(
            statement,
            &|_: &str| false,
            control,
            None,
            Purpose::StoredReferences,
            work,
        )
        .await?;
        observer.plan(plan.as_plan_ref(), control, work).await?;
        let context = plan.0.context().clone();
        plan.0
            .take()
            .into_retirement(context)
            .retire_from(control, work)
            .await
    }
}
