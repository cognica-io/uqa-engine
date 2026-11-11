//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Child, Expr, Input, InputError, Parts, Values};
use crate::{
    ast::{
        stored_input::{boolean, number, optional},
        CompositeRowBinding, CompositeRowLayout,
    },
    expr::composites::{
        CompositeAttribute, CompositeConstantSource, CompositeTypeDescriptor,
        DroppedCompositeAttribute,
    },
};
use uqa_core::{
    json::JsonToken,
    memory::{BudgetedVec, Produced},
};

impl<'a> Input<'a, '_> {
    pub(super) fn typed_literal(
        &mut self,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Produced<Expr>, InputError> {
        let (mut value, mut ty, mut composite_source) = (None, None, None);
        let mut collation_oid = None;
        self.fields_with_defaults(
            token,
            &["value", "ty", "collation_oid", "composite_source"],
            3,
            3,
            |input, index, token| {
                match index {
                    0 => value = Some(values.value(input, token)?),
                    1 => ty = Some(input.text(token)?),
                    2 => collation_oid = optional(token)?,
                    _ => {
                        composite_source = Some(input.maybe(token, |input, token| {
                            input.boxed(token, |input, token| input.composite_source(token, values))
                        })?);
                    }
                }
                Ok(())
            },
        )?;
        let mut parts = Parts::new(self.control);
        let value = Expr::TypedLiteral {
            value: parts.take(value.expect("required literal")),
            ty: parts.take(ty.expect("required declared type")),
            collation_oid,
            composite_source: composite_source.and_then(|value| parts.take(value)),
        };
        parts.finish(value)
    }

    pub(super) fn composite_row(
        &mut self,
        token: JsonToken<'a>,
        children: &mut BudgetedVec<Child<'a>>,
    ) -> Result<Produced<Expr>, InputError> {
        let (mut items, mut binding) = (None, None);
        self.fields(token, &["items", "binding"], 3, |input, index, token| {
            if index == 0 {
                items = Some(input.items(token, children)?);
            } else {
                binding = Some(input.composite_row_binding(token)?);
            }
            Ok(())
        })?;
        let mut parts = Parts::new(self.control);
        let value = Expr::CompositeRow {
            items: parts.take(items.expect("required row items")),
            binding: parts.take(binding.expect("required row binding")),
        };
        parts.finish(value)
    }

    fn composite_row_binding(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<CompositeRowBinding>, InputError> {
        let (mut ty, mut attributes, mut layout, mut argument_types) = (None, None, None, None);
        self.closed_fields(
            token,
            &["ty", "attributes", "layout", "argument_types"],
            [3, 3],
            |input, index, token| {
                match index {
                    0 => ty = Some(input.text(token)?),
                    1 => {
                        attributes =
                            Some(input.array(token, |input, token| {
                                input.scalar(number::<i16>(token)?)
                            })?);
                    }
                    2 if token == JsonToken::Null => (),
                    2 => layout = Some(input.composite_layout(token)?),
                    _ => {
                        argument_types = Some(
                            input.maybe(token, |input, token| input.array(token, Self::column))?,
                        );
                    }
                }
                Ok(())
            },
        )?;
        let mut parts = Parts::new(self.control);
        let value = CompositeRowBinding {
            ty: parts.take(ty.expect("required row type")),
            attributes: parts.take(attributes.expect("required row attributes")),
            layout,
            argument_types: argument_types.and_then(|value| parts.take(value)),
        };
        parts.finish(value)
    }

    fn composite_layout(&mut self, token: JsonToken<'a>) -> Result<CompositeRowLayout, InputError> {
        let (mut width, mut implicit) = (0, false);
        self.closed_fields(token, &["width", "implicit"], [3, 3], |_, index, token| {
            if index == 0 {
                width = number(token)?;
            } else {
                implicit = boolean(token)?;
            }
            Ok(())
        })?;
        Ok(CompositeRowLayout { width, implicit })
    }

    fn composite_source(
        &mut self,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Produced<CompositeConstantSource>, InputError> {
        let (mut value, mut descriptors) = (None, None);
        self.fields(
            token,
            &["value", "descriptors"],
            3,
            |input, index, token| {
                if index == 0 {
                    value = Some(values.value(input, token)?);
                } else {
                    descriptors = Some(input.array(token, Self::composite_descriptor)?);
                }
                Ok(())
            },
        )?;
        let mut parts = Parts::new(self.control);
        let value = CompositeConstantSource {
            value: parts.take(value.expect("required original value")),
            descriptors: parts.take(descriptors.expect("required original descriptors")),
        };
        parts.finish(value)
    }

    fn composite_descriptor(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<CompositeTypeDescriptor>, InputError> {
        let (mut dropped, mut attributes) = (None, None);
        let (mut type_oid, mut relation_oid) = (0, 0);
        self.fields(
            token,
            &["dropped", "type_oid", "relation_oid", "attributes"],
            14,
            |input, index, token| {
                match index {
                    0 => {
                        dropped = Some(input.array(token, |input, token| {
                            let value = input.dropped_attribute(token)?;
                            input.scalar(value)
                        })?);
                    }
                    1 => type_oid = number(token)?,
                    2 => relation_oid = number(token)?,
                    _ => attributes = Some(input.array(token, Self::composite_attribute)?),
                }
                Ok(())
            },
        )?;
        let mut parts = Parts::new(self.control);
        let value = CompositeTypeDescriptor {
            type_oid,
            relation_oid,
            dropped: dropped.map(|value| parts.take(value)).unwrap_or_default(),
            attributes: parts.take(attributes.expect("required original attributes")),
        };
        parts.finish(value)
    }

    fn composite_attribute(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<CompositeAttribute>, InputError> {
        let (mut name, mut ty) = (None, None);
        let (mut collation_oid, mut number_value) = (None, 0);
        self.fields(
            token,
            &["name", "ty", "collation_oid", "number"],
            11,
            |input, index, token| {
                match index {
                    0 => name = Some(input.text(token)?),
                    1 => ty = Some(input.column(token)?),
                    2 => collation_oid = optional(token)?,
                    _ => number_value = number(token)?,
                }
                Ok(())
            },
        )?;
        let mut parts = Parts::new(self.control);
        let value = CompositeAttribute {
            name: parts.take(name.expect("required attribute name")),
            ty: parts.take(ty.expect("required attribute type")),
            collation_oid,
            number: number_value,
        };
        parts.finish(value)
    }

    fn dropped_attribute(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<DroppedCompositeAttribute, InputError> {
        let (mut number_value, mut length, mut alignment) = (0, 0, 0);
        self.fields(
            token,
            &["number", "length", "alignment"],
            7,
            |_, index, token| {
                match index {
                    0 => number_value = number(token)?,
                    1 => length = number(token)?,
                    _ => alignment = number(token)?,
                }
                Ok(())
            },
        )?;
        Ok(DroppedCompositeAttribute {
            number: number_value,
            length,
            alignment,
        })
    }
}
