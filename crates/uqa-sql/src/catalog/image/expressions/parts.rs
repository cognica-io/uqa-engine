//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Child positions retain exact order without prewalking syntax, vectors or descriptors.

use super::{Child, DefinitionImageReader, Frame, SQLError, ValueImageReader};
use crate::ast::{CompositeRowBinding, NullsOrder};

impl<'a> DefinitionImageReader<'a> {
    pub(super) fn advance(&mut self, frame: Frame<'a>) -> Result<(), SQLError> {
        match frame {
            Frame::Plan(frame) => self.plan_part(frame)?,
            Frame::NextView => self.next_view()?,
            Frame::Scalar(part) => self.scalar_part(part)?,
            Frame::Table(part) => self.table_part(part)?,
            Frame::NextDomain
            | Frame::DomainDefinition(_)
            | Frame::DomainChecks(_)
            | Frame::DomainNotNull(_)
            | Frame::ObjectAcls(_) => self.domain_part(frame)?,
            Frame::Node(value) => self.node(value)?,
            Frame::Nodes(values) => {
                if let Some((first, rest)) = values.split_first() {
                    self.push(Frame::Nodes(rest))?;
                    self.push(Frame::Node(first))?;
                }
            }
            Frame::Orders(values) => {
                if let Some((first, rest)) = values.split_first() {
                    self.fields.flag(first.descending);
                    self.fields.scalar([match first.nulls {
                        None => 0,
                        Some(NullsOrder::First) => 1,
                        Some(NullsOrder::Last) => 2,
                    }]);
                    self.push(Frame::Orders(rest))?;
                    self.push(Frame::Node(&first.expr))?;
                }
            }
            Frame::Cases(values) => {
                if let Some(((condition, result), rest)) = values.split_first() {
                    self.push(Frame::Cases(rest))?;
                    self.push(Frame::Node(result))?;
                    self.push(Frame::Node(condition))?;
                }
            }
            Frame::Binding(value) => {
                let memory = self
                    .memory
                    .reserve(size_of::<super::CatalogImageReader<'_>>())?;
                self.child = Some(Child::Binding {
                    reader: Box::new(value.image_reader(self.cancellation)?),
                    _memory: memory,
                });
            }
            Frame::Value(value) => {
                let memory = self.memory.reserve(size_of::<ValueImageReader<'_, '_>>())?;
                self.child = Some(Child::Value {
                    reader: Box::new(ValueImageReader::new(value, self.memory, self.cancellation)),
                    _memory: memory,
                });
            }
            Frame::Type(value) => {
                if let Some(child) = self.fields.column_type(value) {
                    self.push(Frame::Type(child))?;
                }
            }
            Frame::Types(values) => {
                if let Some((first, rest)) = values.split_first() {
                    self.push(Frame::Types(rest))?;
                    self.push(Frame::Type(first))?;
                }
            }
            Frame::AttributeNumbers(values) => {
                if let Some((first, rest)) = values.split_first() {
                    self.fields.scalar(first.to_le_bytes());
                    self.push(Frame::AttributeNumbers(rest))?;
                }
            }
            other => self.composite_parts(other)?,
        }
        Ok(())
    }

    fn composite_parts(&mut self, frame: Frame<'a>) -> Result<(), SQLError> {
        match frame {
            Frame::CompositeSource(value) => {
                self.fields.count(value.descriptors.len());
                self.push(Frame::Descriptors(&value.descriptors))?;
                self.push(Frame::Value(&value.value))?;
            }
            Frame::Descriptors(values) => {
                if let Some((first, rest)) = values.split_first() {
                    self.fields.scalar(first.type_oid.to_le_bytes());
                    self.fields.scalar(first.relation_oid.to_le_bytes());
                    self.fields.count(first.attributes.len());
                    self.fields.count(first.dropped.len());
                    self.push(Frame::Descriptors(rest))?;
                    self.push(Frame::Dropped(&first.dropped))?;
                    self.push(Frame::Attributes(&first.attributes))?;
                }
            }
            Frame::Attributes(values) => {
                if let Some((first, rest)) = values.split_first() {
                    self.fields.text(&first.name);
                    self.fields.optional_u32(first.collation_oid);
                    self.fields.scalar(first.number.to_le_bytes());
                    self.push(Frame::Attributes(rest))?;
                    self.push(Frame::Type(&first.ty))?;
                }
            }
            Frame::Dropped(values) => {
                if let Some((first, rest)) = values.split_first() {
                    self.fields.scalar(first.number.to_le_bytes());
                    self.fields.scalar(first.length.to_le_bytes());
                    self.fields.scalar([first.alignment]);
                    self.push(Frame::Dropped(rest))?;
                }
            }
            _ => unreachable!("composite image continuation"),
        }
        Ok(())
    }

    pub(super) fn row_binding(&mut self, value: &'a CompositeRowBinding) -> Result<(), SQLError> {
        let CompositeRowBinding {
            ty,
            attributes,
            layout,
            argument_types,
        } = value;
        self.fields.text(ty);
        self.fields.count(attributes.len());
        self.fields.flag(layout.is_some());
        if let Some(layout) = layout {
            self.fields.scalar(layout.width.to_le_bytes());
            self.fields.flag(layout.implicit);
        }
        self.fields.flag(argument_types.is_some());
        if let Some(types) = argument_types {
            self.fields.count(types.len());
            self.push(Frame::Types(types))?;
        }
        self.push(Frame::AttributeNumbers(attributes))?;
        Ok(())
    }
}
