//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Replaced and resolved specifications share scalar retirement before their owning leases are released.

use super::Pending;
use crate::{
    ir::ownership::Retirement, plan::scalar::ScalarInputWork as Work, ScalarExpr, ScalarWindowSpec,
};
use uqa_core::memory::ProductionControl;

pub(in crate::plan) struct WindowRetirement {
    specification: Option<Pending<ScalarWindowSpec>>,
    payload: Option<Retirement>,
}

impl WindowRetirement {
    pub(in crate::plan) fn new(specification: ScalarWindowSpec) -> Self {
        Self {
            specification: Some(Pending::new(specification)),
            payload: None,
        }
    }

    pub(in crate::plan) async fn retire_from<W: Work>(
        &mut self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), W::Error> {
        let specification = self.specification.as_mut().expect("retiring window");
        for expression in specification.expressions_mut() {
            checkpoint(control, work).await?;
            self.payload = Some(Retirement::new(std::mem::replace(
                expression,
                ScalarExpr::Star,
            )));
            loop {
                checkpoint(control, work).await?;
                if self.payload.as_mut().expect("retiring scalar").advance() {
                    break;
                }
            }
            self.payload = None;
        }
        while !specification.partition_by.is_empty() {
            checkpoint(control, work).await?;
            specification.partition_by.pop();
        }
        while !specification.order_by.is_empty() {
            checkpoint(control, work).await?;
            specification.order_by.pop();
        }
        drop(self.specification.take());
        Ok(())
    }
}

pub(in crate::plan) async fn checkpoint<W: Work>(
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), W::Error> {
    control.check()?;
    work.checkpoint().await?;
    control.check()?;
    Ok(())
}
