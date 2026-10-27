//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exclusive ownership of an input while its parent pipeline is constructed.

use super::{physical_exec_error, PhysicalOperator};
use uqa_sql::SQLError;

type Input<'a> = Box<dyn PhysicalOperator + 'a>;

/// Construction retains its input until an infallible wrapper takes ownership
/// or another construction boundary accepts it. A failed preparation closes
/// the current pipeline before returning its original diagnostic.
pub(crate) struct OperatorBuild<'a> {
    input: Option<Input<'a>>,
}

impl<'a> OperatorBuild<'a> {
    pub(crate) fn build(
        input: Input<'a>,
        build: impl FnOnce(&mut Self) -> Result<(), SQLError>,
    ) -> Result<Input<'a>, SQLError> {
        Self::run(input, build).map(|(input, ())| input)
    }

    pub(crate) fn run<T>(
        input: Input<'a>,
        build: impl FnOnce(&mut Self) -> Result<T, SQLError>,
    ) -> Result<(Input<'a>, T), SQLError> {
        let mut owner = Self { input: Some(input) };
        match build(&mut owner) {
            Ok(value) => Ok((owner.take(), value)),
            Err(error) => Err(match owner.close() {
                Ok(()) => error,
                Err(cleanup) => {
                    error.with_cleanup(cleanup, "close input after construction failure")
                }
            }),
        }
    }

    pub(crate) fn pair<T>(
        left: Input<'a>,
        right: Input<'a>,
        build: impl FnOnce(&mut Self, &mut Self) -> Result<T, SQLError>,
    ) -> Result<(Input<'a>, Input<'a>, T), SQLError> {
        let (left, (right, value)) =
            Self::run(left, |left| Self::run(right, |right| build(left, right)))?;
        Ok((left, right, value))
    }

    /// Attach an infallible parent after all of its fallible inputs are ready.
    pub(crate) fn wrap(&mut self, wrap: impl FnOnce(Input<'a>) -> Input<'a>) {
        self.input = Some(wrap(self.take()));
    }

    /// Transfer the input to a construction boundary that owns cleanup on
    /// failure. It must return the completed pipeline on success.
    pub(crate) fn attach(
        &mut self,
        build: impl FnOnce(Input<'a>) -> Result<Input<'a>, SQLError>,
    ) -> Result<(), SQLError> {
        self.input = Some(build(self.take())?);
        Ok(())
    }

    fn take(&mut self) -> Input<'a> {
        self.input.take().expect("construction owns its input")
    }

    fn close(&mut self) -> Result<(), SQLError> {
        self.input.take().map_or(Ok(()), |mut input| {
            input.close().map_err(physical_exec_error)
        })
    }
}

impl<'a> std::ops::Deref for OperatorBuild<'a> {
    type Target = dyn PhysicalOperator + 'a;

    fn deref(&self) -> &Self::Target {
        self.input.as_deref().expect("construction owns its input")
    }
}

impl Drop for OperatorBuild<'_> {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
mod tests;
