//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{boxed_from, map_from, Lowering, Work};
use crate::{
    ast::CompositeRowBinding,
    expr::composites::{CompositeAttribute, CompositeConstantSource, CompositeTypeDescriptor},
};

impl Lowering<'_> {
    pub(in crate::plan::scalar) async fn composite_binding_from<W: Work>(
        &mut self,
        binding: &CompositeRowBinding,
        work: &mut W,
    ) -> Result<CompositeRowBinding, W::Error> {
        self.checkpoint(work).await?;
        Ok(CompositeRowBinding {
            layout: binding.layout,
            ty: self.text_from(&binding.ty, work).await?,
            attributes: map_from!(self, binding.attributes.iter(), work, |number| *number),
            argument_types: match &binding.argument_types {
                Some(types) => Some(map_from!(self, types.iter(), work, |ty| {
                    self.type_from(Some(ty), work)
                        .await?
                        .expect("constructor argument type")
                })),
                None => None,
            },
        })
    }

    pub(in crate::plan::scalar) async fn composite_source_from<W: Work>(
        &mut self,
        source: Option<&CompositeConstantSource>,
        work: &mut W,
    ) -> Result<Option<Box<CompositeConstantSource>>, W::Error> {
        Ok(match source {
            Some(source) => Some(boxed_from!(
                self,
                work,
                CompositeConstantSource,
                CompositeConstantSource {
                    value: self.value_from(&source.value, work).await?,
                    descriptors: map_from!(self, source.descriptors.iter(), work, |descriptor| {
                        CompositeTypeDescriptor {
                            dropped: map_from!(
                                self,
                                descriptor.dropped.iter(),
                                work,
                                |attribute| *attribute
                            ),
                            type_oid: descriptor.type_oid,
                            relation_oid: descriptor.relation_oid,
                            attributes: map_from!(
                                self,
                                descriptor.attributes.iter(),
                                work,
                                |attribute| {
                                    CompositeAttribute {
                                        collation_oid: attribute.collation_oid,
                                        name: self.text_from(&attribute.name, work).await?,
                                        ty: self
                                            .type_from(Some(&attribute.ty), work)
                                            .await?
                                            .expect("source attribute type"),
                                        number: attribute.number,
                                    }
                                }
                            ),
                        }
                    }),
                }
            )),
            None => None,
        })
    }
}

impl CompositeRowBinding {
    pub(crate) async fn clone_with_input_work<W: Work>(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
        work: &mut W,
    ) -> Result<uqa_core::memory::Produced<Self>, W::Error> {
        let mut copying = Lowering {
            control: control
                .budget()
                .map(|_| super::super::Control::from_production(*control)),
            owned_children: None,
        };
        let value = copying.composite_binding_from(self, work).await?;
        Ok(control.finish(value, copying.control.map(|owner| owner.memory))?)
    }
}
