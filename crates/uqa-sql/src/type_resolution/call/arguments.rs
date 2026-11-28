//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporary argument slots keep their own capacity separate from independently owned scalar payloads.

use crate::{
    plan::{OwnedScalarExpr, ScalarInputWork},
    schema::retention::CatalogRetentionError,
    SQLError,
};
use uqa_core::memory::{MemoryReservation, ProductionControl, ProductionVec};

pub(in crate::type_resolution) struct Arguments<'a> {
    values: ProductionVec<'a, OwnedScalarExpr>,
    control: ProductionControl<'a>,
}

struct ArgumentIterator {
    values: std::vec::IntoIter<OwnedScalarExpr>,
    _memory: Option<MemoryReservation>,
}

impl Iterator for ArgumentIterator {
    type Item = OwnedScalarExpr;
    fn next(&mut self) -> Option<Self::Item> {
        self.values.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.values.size_hint()
    }
}
impl ExactSizeIterator for ArgumentIterator {}

impl<'a> Arguments<'a> {
    pub(in crate::type_resolution) fn new(
        count: usize,
        control: &ProductionControl<'a>,
    ) -> Result<Self, SQLError> {
        let mut values = ProductionVec::new(*control);
        values.reserve(count)?;
        Ok(Self {
            values,
            control: *control,
        })
    }

    pub(in crate::type_resolution) fn push(
        &mut self,
        argument: OwnedScalarExpr,
    ) -> Result<(), SQLError> {
        Ok(self.values.push_produced(
            self.control
                .finish(argument, self.control.empty_reservation())?,
        )?)
    }

    pub(in crate::type_resolution) fn finish(self) -> Result<OwnedScalarExpr, SQLError> {
        Ok(crate::catalog::work::inline(
            self.finish_from(false, &mut ()),
        )?)
    }

    pub(in crate::type_resolution) async fn finish_from<W: ScalarInputWork>(
        self,
        array: bool,
        work: &mut W,
    ) -> Result<OwnedScalarExpr, W::Error>
    where
        W::Error: From<CatalogRetentionError>,
    {
        let (values, memory) = self.values.finish()?.into_parts();
        let children = ArgumentIterator {
            values: values.into_iter(),
            _memory: memory,
        };
        if array {
            OwnedScalarExpr::array_from(children, &self.control, work).await
        } else {
            OwnedScalarExpr::boolean_from(children, false, &self.control, &self.control, work).await
        }
    }
}
