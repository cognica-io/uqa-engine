//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A privately bound statement keeps its original lease through role observation and canonical construction.

use crate::{
    catalog::regrole_dependencies::StoredRegroleConstants,
    construction::{Driver, Read},
    plan::{copy::stored_plan, PlanRef, UnifiedPlan},
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

pub(super) enum Stage {
    Roles,
    Fields,
}

pub(super) enum Completed {
    Roles(Produced<StoredRegroleConstants>, stored_plan::BoundPlan),
    Plan(Produced<UnifiedPlan>, usize),
}

pub(super) struct Statement(Read<'static, Result<Completed, SQLError>>);

impl Statement {
    pub(super) fn new(
        source: stored_plan::BoundPlan,
        stage: Stage,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let retained = control.capture();
        Ok(Self(Read::new(*control, async move {
            let control = retained.control();
            match stage {
                Stage::Roles => {
                    let roles = {
                        let mut driver = Driver::new(control)?;
                        let plan = PlanRef::from(&*source);
                        let work = driver.work().submit(move |mut work| async move {
                            StoredRegroleConstants::from_plan(plan, &control, &mut work).await
                        })?;
                        driver.complete(work).await??
                    };
                    Ok(Completed::Roles(roles, source))
                }
                Stage::Fields => {
                    let (plan, children) = {
                        let mut driver = Driver::new(control)?;
                        let field_source = &*source;
                        let work = driver.work().submit(move |work| {
                            stored_plan::capture(field_source, control, work)
                        })?;
                        driver.complete(work).await??
                    };
                    let mut driver = Driver::new(control)?;
                    let work = driver.work().submit(move |mut work| async move {
                        source.retire(&control, &mut work).await
                    })?;
                    driver.complete(work).await??;
                    Ok(Completed::Plan(plan, children))
                }
            }
        })?))
    }

    pub(super) fn advance(
        &mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Completed>, SQLError> {
        self.0.advance_window(control, 32)
    }
}
