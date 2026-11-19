//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The immutable placeholder owns every borrowed input until its private parameter and exact field receipt are complete.

use super::{FunctionParam, Result, SQLRoutine};
use crate::{
    construction,
    plan::copy::syntax::native::{Field, Input},
};
use uqa_core::memory::{Produced, ProductionControl};

type Parameter = (Produced<FunctionParam>, usize);
type ParameterDriver<'a> = (
    construction::Driver<'a>,
    construction::Output<Result<Parameter>>,
);

pub(super) struct Capture(construction::Read<'static, Result<Parameter>>);

impl Capture {
    pub(super) fn new(
        source: SQLRoutine,
        position: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        let retained = control.capture();
        Ok(Self(construction::Read::new(*control, async move {
            let control = retained.control();
            let parameter = &source.def.params[position];
            let (mut driver, output) = start(parameter, control)?;
            driver.complete(output).await?
        })?))
    }

    pub(super) fn advance(&mut self, control: &ProductionControl<'_>) -> Result<Option<Parameter>> {
        self.0.advance_window(control, 32)
    }
}

fn start<'a>(
    parameter: &'a FunctionParam,
    control: ProductionControl<'a>,
) -> Result<ParameterDriver<'a>> {
    let driver = construction::Driver::new(control)?;
    let output = driver.work().submit(move |work| async move {
        let mut fields = Input::new(control, work);
        let value = parameter.copy_from(&mut fields).await?;
        fields.finish(value)
    })?;
    Ok((driver, output))
}

/// Copy the selected current syntax while its owning edit remains pending.
pub(super) async fn capture_from(
    parameter: &FunctionParam,
    control: &ProductionControl<'_>,
    work: &mut (impl crate::catalog::work::Work + Send),
) -> Result<Parameter> {
    let (mut driver, output) = start(parameter, *control)?;
    while driver.advance().await? {
        work.checkpoint().await?;
    }
    construction::receive(output).await
}
