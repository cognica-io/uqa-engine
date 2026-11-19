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
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use uqa_core::memory::{Produced, ProductionControl};

type Parameter = (Produced<FunctionParam>, usize);

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
            let mut driver = construction::Driver::new(control)?;
            let output = driver.work().submit(move |work| async move {
                let mut fields = Input::new(control, work);
                let value = parameter.copy_from(&mut fields).await?;
                fields.finish(value)
            })?;
            while driver.advance().await? {
                construction::suspend().await;
            }
            construction::receive(output).await
        })?))
    }

    pub(super) fn advance(&mut self, control: &ProductionControl<'_>) -> Result<Option<Parameter>> {
        let mut context = Context::from_waker(Waker::noop());
        for _ in 0..32 {
            control.check()?;
            if let Poll::Ready(value) = Pin::new(&mut self.0).poll(&mut context) {
                return value.map(Some);
            }
        }
        Ok(None)
    }
}
