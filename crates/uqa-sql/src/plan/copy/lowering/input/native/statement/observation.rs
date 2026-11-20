//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{ProductionControl, Purpose, RetainedExecutablePlan, UnifiedPlan};
use crate::{
    ast::Statement,
    catalog::work::Work,
    plan::{MutableExecutablePlan, PlanRef},
    SQLError,
};
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

pub(crate) enum StoredStatementPlan {
    Ordinary(MutableExecutablePlan),
    Retained(RetainedExecutablePlan),
}

impl StoredStatementPlan {
    pub(crate) fn as_plan_ref(&self) -> PlanRef<'_> {
        match self {
            Self::Ordinary(plan) => plan.as_plan_ref(),
            Self::Retained(plan) => plan.as_plan_ref(),
        }
    }

    pub(crate) async fn retire<W: Work + Send>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        if let Self::Retained(plan) = self {
            let context = plan.0.context().clone();
            plan.0
                .take()
                .into_retirement(context)
                .retire_from(control, work)
                .await?;
        }
        Ok(())
    }
}

impl UnifiedPlan {
    /// Lower a stored statement, visit its native fields, and retire its temporary tree and field leases on the caller's work schedule.
    pub async fn visit_stored_statement_with_work<W: Work + Send>(
        statement: &Statement,
        control: &ProductionControl<'_>,
        work: &mut W,
        observer: &mut (impl StoredStatementObserver + Send),
    ) -> Result<(), SQLError> {
        let plan = Self::lower_stored_statement_with_work(statement, control, work).await?;
        observer.plan(plan.as_plan_ref(), control, work).await?;
        plan.retire(control, work).await
    }

    pub(crate) async fn lower_stored_statement_with_work<W: Work + Send>(
        statement: &Statement,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<StoredStatementPlan, SQLError> {
        if !W::MAY_SUSPEND || control.budget().is_none() {
            return Self::lower_borrowed_for_rewrite(statement, &|_: &str| false, control)
                .map(StoredStatementPlan::Ordinary);
        }
        Self::lower_retained_statement(
            statement,
            &|_: &str| false,
            control,
            None,
            Purpose::StoredReferences,
            work,
        )
        .await
        .map(StoredStatementPlan::Retained)
    }
}
