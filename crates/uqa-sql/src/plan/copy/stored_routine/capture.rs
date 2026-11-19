//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The immutable routine owns each borrowed field until the private syntax and its receipt are complete.

use super::{CreateFunction, FunctionParam, Result, SQLRoutine, Statement};
use crate::{
    catalog::work::Work,
    construction,
    plan::copy::syntax::native::{Field, Input, Nested, Window},
};
use uqa_core::memory::ProductionControl;

type FieldDriver<'a, T> = (
    construction::Driver<'a>,
    construction::Output<Result<Nested<T>>>,
);

pub(super) struct Capture<T>(construction::Read<'static, Result<Nested<T>>>);

#[derive(Default)]
pub(super) struct Pending {
    pub(super) parameter: Option<Capture<FunctionParam>>,
    pub(super) statement: Option<Capture<Statement>>,
}

impl<T: Field + 'static> Capture<T> {
    pub(super) fn next(
        slot: &mut Option<Self>,
        source: &SQLRoutine,
        position: usize,
        select: fn(&CreateFunction, usize) -> &T,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Nested<T>>> {
        let mut capture = if let Some(capture) = slot.take() {
            capture
        } else {
            if let Some(value) = select(&source.def, position).capture_inline(control)? {
                return Ok(Some(value));
            }
            let source = source.clone();
            let retained = control.capture();
            Self(construction::Read::new(*control, async move {
                let (mut driver, output) =
                    start(select(&source.def, position), retained.control())?;
                driver.complete(output).await?
            })?)
        };
        let output = capture.0.advance_window(control, Window::STEPS)?;
        if output.is_none() {
            *slot = Some(capture);
        }
        Ok(output)
    }
}

fn start<'a, T: Field>(value: &'a T, control: ProductionControl<'a>) -> Result<FieldDriver<'a, T>> {
    let driver = construction::Driver::new(control)?;
    let output = driver.work().submit(move |work| async move {
        let mut fields = Input::new(control, work);
        let value = value.copy_from(&mut fields).await?;
        fields.finish_nested(value)
    })?;
    Ok((driver, output))
}

/// Copy the selected current syntax while its owning edit remains pending.
pub(super) async fn from<T: Field>(
    value: &T,
    control: &ProductionControl<'_>,
    work: &mut (impl Work + Send),
) -> Result<Nested<T>> {
    if let Some(value) = value.capture_inline(control)? {
        return Ok(value);
    }
    let (mut driver, output) = start(value, *control)?;
    while driver.advance().await? {
        work.checkpoint().await?;
    }
    construction::receive(output).await
}
