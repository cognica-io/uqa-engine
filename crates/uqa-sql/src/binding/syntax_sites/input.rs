//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain the two position lists and their originally selected metadata until their syntax has consumed every site.

use super::{
    AsyncImageInput, BoundRoutineReference, Produced, ProductionControl, ProductionVec, SQLError,
    SyntaxSites, Value, ValueSite,
};
use crate::{
    ast::{CompositeRowBinding, FunctionBinding},
    plan::ScalarInputWork,
};

pub(in crate::binding) trait SiteInput: ScalarInputWork {
    const MAY_SUSPEND: bool;
}

impl SiteInput for () {
    const MAY_SUSPEND: bool = false;
}

impl SiteInput for AsyncImageInput<'_> {
    const MAY_SUSPEND: bool = true;
}

pub(super) struct Walk<'a, 'c, W> {
    pub(super) control: ProductionControl<'c>,
    work: &'a mut W,
    values: ProductionVec<'c, ValueSite>,
    routines: ProductionVec<'c, BoundRoutineReference>,
}

impl<'a, 'c, W: SiteInput> Walk<'a, 'c, W>
where
    W::Error: Into<SQLError>,
{
    pub(super) fn new(control: ProductionControl<'c>, work: &'a mut W) -> Self {
        Self {
            control,
            work,
            values: ProductionVec::new(control),
            routines: ProductionVec::new(control),
        }
    }

    pub(super) async fn checkpoint(&mut self) -> Result<(), SQLError> {
        self.control.check()?;
        self.work.checkpoint().await.map_err(Into::into)?;
        Ok(self.control.check()?)
    }

    pub(super) fn plain(&mut self, value: ValueSite) -> Result<(), SQLError> {
        Ok(self.values.push_produced(
            self.control
                .finish(value, self.control.empty_reservation())?,
        )?)
    }

    pub(super) async fn text(
        &mut self,
        text: &str,
        field: fn(String) -> ValueSite,
    ) -> Result<(), SQLError> {
        let (text, receipt) = self
            .work
            .text(text, &self.control)
            .await
            .map_err(Into::into)?
            .into_parts();
        Ok(self
            .values
            .push_produced(self.control.finish(field(text), receipt)?)?)
    }

    pub(super) async fn constant(&mut self, value: &Value, ty: &str) -> Result<(), SQLError> {
        let (value, payload) = self
            .work
            .value(value, &self.control)
            .await
            .map_err(Into::into)?
            .into_parts();
        let (ty, spelling) = self
            .work
            .text(ty, &self.control)
            .await
            .map_err(Into::into)?
            .into_parts();
        Ok(self.values.push_produced(self.control.finish(
            ValueSite::Constant { value, ty },
            self.control.combine(payload, spelling),
        )?)?)
    }

    pub(super) async fn composite(
        &mut self,
        binding: &CompositeRowBinding,
    ) -> Result<(), SQLError> {
        let (binding, lease) = binding
            .clone_with_input_work(&self.control, self.work)
            .await
            .map_err(Into::into)?
            .into_parts();
        Ok(self.values.push_produced(
            self.control
                .finish(ValueSite::CompositeRow(binding), lease)?,
        )?)
    }

    pub(super) async fn membership(&mut self, items: &[usize]) -> Result<(), SQLError> {
        let mut indexes = ProductionVec::new(self.control);
        indexes.reserve(items.len())?;
        for chunk in items.chunks(4096 / size_of::<usize>()) {
            self.checkpoint().await?;
            indexes.extend_from_slice(chunk)?;
        }
        let (array_items, lease) = indexes.finish()?.into_parts();
        Ok(self.values.push_produced(
            self.control
                .finish(ValueSite::Membership { array_items }, lease)?,
        )?)
    }

    pub(super) async fn routine(
        &mut self,
        name: &str,
        binding: Option<&FunctionBinding>,
    ) -> Result<(), SQLError> {
        let (name, spelling) = self
            .work
            .text(name, &self.control)
            .await
            .map_err(Into::into)?
            .into_parts();
        let (binding, declaration) = if let Some(binding) = binding {
            let (binding, declaration) = binding
                .clone_with_input_work(&self.control, self.work)
                .await
                .map_err(Into::into)?
                .into_parts();
            (Some(binding), declaration)
        } else {
            (None, self.control.empty_reservation())
        };
        Ok(self.routines.push_produced(self.control.finish(
            BoundRoutineReference { name, binding },
            self.control.combine(spelling, declaration),
        )?)?)
    }

    pub(super) fn finish(self) -> Result<Produced<SyntaxSites>, SQLError> {
        let (values, values_memory) = self.values.finish()?.into_parts();
        let (routines, routines_memory) = self.routines.finish()?.into_parts();
        Ok(self.control.finish(
            SyntaxSites { routines, values },
            self.control.combine(routines_memory, values_memory),
        )?)
    }
}
