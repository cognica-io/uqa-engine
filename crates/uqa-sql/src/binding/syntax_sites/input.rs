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

pub(crate) trait SiteInput: ScalarInputWork + uqa_core::ValueCopyWork {
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
        ScalarInputWork::checkpoint(self.work)
            .await
            .map_err(Into::into)?;
        Ok(self.control.check()?)
    }

    async fn append<T: Send>(
        buffer: &mut ProductionVec<'c, T>,
        value: Produced<T>,
        control: ProductionControl<'c>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        if W::MAY_SUSPEND && buffer.push_needs_work() {
            let original = std::mem::replace(buffer, ProductionVec::new(control));
            *buffer = crate::construction::retain_with(control, || {
                original.into_reserved_for_push(work)
            })?
            .await?;
        }
        Ok(buffer.push_produced(value)?)
    }

    async fn value_site(&mut self, value: Produced<ValueSite>) -> Result<(), SQLError> {
        Self::append(&mut self.values, value, self.control, self.work).await
    }

    pub(super) async fn plain(&mut self, value: ValueSite) -> Result<(), SQLError> {
        self.value_site(
            self.control
                .finish(value, self.control.empty_reservation())?,
        )
        .await
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
        self.value_site(self.control.finish(field(text), receipt)?)
            .await
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
        self.value_site(self.control.finish(
            ValueSite::Constant { value, ty },
            self.control.combine(payload, spelling),
        )?)
        .await
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
        self.value_site(
            self.control
                .finish(ValueSite::CompositeRow(binding), lease)?,
        )
        .await
    }

    pub(super) async fn membership(&mut self, items: &[usize]) -> Result<(), SQLError> {
        let mut indexes = ProductionVec::new(self.control);
        indexes.reserve(items.len())?;
        for chunk in items.chunks(4096 / size_of::<usize>()) {
            self.checkpoint().await?;
            indexes.extend_from_slice(chunk)?;
        }
        let (array_items, lease) = indexes.finish()?.into_parts();
        self.value_site(
            self.control
                .finish(ValueSite::Membership { array_items }, lease)?,
        )
        .await
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
        let reference = self.control.finish(
            BoundRoutineReference { name, binding },
            self.control.combine(spelling, declaration),
        )?;
        Self::append(&mut self.routines, reference, self.control, self.work).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::FunctionOrderSyntax;
    use uqa_core::{
        image_io::{ImageDecodeWindow, ImageDecoder},
        memory::MemoryBudget,
        CancellationToken,
    };

    #[test]
    fn retained_syntax_sites_pause_while_relocating_existing_metadata_and_keep_the_last_site() {
        for (width, failure) in [
            (1, None),
            (4096, None),
            (4096, Some("57014")),
            (4096, Some("53200")),
        ] {
            let memory = MemoryBudget::new(2 << 20);
            let token = CancellationToken::new();
            let native = memory.clone();
            let mut reader = ImageDecoder::new(&memory, &token, move |mut source| async move {
                let mut input = source.input();
                let control =
                    ProductionControl::new(&native, input.cancellation, input.cancellation);
                let mut walk = Walk::new(control, &mut input);
                walk.values.reserve(width)?;
                let (name, lease) = control.copy_text("saved_type")?.into_parts();
                walk.values
                    .push_produced(control.finish(ValueSite::Cast(name), lease)?)?;
                for _ in 1..width {
                    walk.plain(ValueSite::Node).await?;
                }
                let _occupied =
                    (failure == Some("53200")).then(|| native.reserve(native.available()).unwrap());
                walk.plain(ValueSite::FunctionOrder(FunctionOrderSyntax::WithinGroup))
                    .await?;
                walk.finish()
            })
            .unwrap();
            let mut calls = 0;
            let result = loop {
                calls += 1;
                if failure == Some("57014") && calls == 4 {
                    token.cancel();
                }
                match reader.advance(
                    &mut std::io::empty(),
                    ImageDecodeWindow { bytes: 1, steps: 1 },
                ) {
                    Ok(Some(result)) => break Ok(result),
                    Ok(None) => (),
                    Err(error) => break Err(error),
                }
            };
            if let Some(state) = failure {
                assert_eq!(result.unwrap_err().sqlstate(), Some(state));
            } else {
                let sites = result.unwrap();
                assert_eq!(sites.values.len(), width + 1);
                assert_eq!(sites.values[0], ValueSite::Cast("saved_type".into()));
                assert!(sites.values[1..width]
                    .iter()
                    .all(|site| matches!(site, ValueSite::Node)));
                assert_eq!(
                    sites.values[width],
                    ValueSite::FunctionOrder(FunctionOrderSyntax::WithinGroup)
                );
                let chunk = 4096 / size_of::<ValueSite>();
                assert_eq!(
                    calls,
                    if width == 1 {
                        1
                    } else {
                        1 + width.div_ceil(chunk)
                    }
                );
            }
            drop(reader);
            assert_eq!(memory.used(), 0);
        }
    }
}
