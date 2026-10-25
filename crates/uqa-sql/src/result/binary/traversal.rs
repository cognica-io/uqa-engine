//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Iterative binary output keeps container continuations off the native stack.

use super::{validate_type, ColumnType, Encoder, SQLError, Value};
use crate::{expr::RangeComparisonValue, SQLParam};
use uqa_core::memory::{BudgetedVec, MemoryError, Produced, ProductionControl};

pub(super) enum Input<'a> {
    Borrowed(&'a Value, &'a ColumnType),
    Owned(Produced<SQLParam>),
}

impl Input<'_> {
    pub(super) fn parts(&self) -> (&Value, &ColumnType) {
        match self {
            Self::Borrowed(value, ty) => (value, ty),
            Self::Owned(parameter) => (
                parameter.scalar_value().expect("binary field is scalar"),
                parameter
                    .declared_scalar_type()
                    .unwrap_or(&ColumnType::Text),
            ),
        }
    }
}

pub(super) enum Frame<'a> {
    Value(Input<'a>),
    Field(Produced<SQLParam>, bool),
    Finish(usize),
    Record {
        input: Input<'a>,
        index: usize,
    },
    Array {
        input: Input<'a>,
        element: Produced<ColumnType>,
        index: usize,
        null_position: usize,
    },
    Range {
        range: RangeComparisonValue,
        element: Produced<ColumnType>,
        index: usize,
    },
}

pub(super) enum Stack<'a> {
    Ordinary(Vec<Frame<'a>>),
    Controlled(BudgetedVec<Frame<'a>>),
}

impl<'a> Stack<'a> {
    pub(super) fn push(&mut self, frame: Frame<'a>) -> Result<(), SQLError> {
        match self {
            Self::Ordinary(frames) => {
                frames.try_reserve(1).map_err(MemoryError::from)?;
                frames.push(frame);
            }
            Self::Controlled(frames) => frames.push(frame)?,
        }
        Ok(())
    }

    fn pop(&mut self) -> Option<Frame<'a>> {
        match self {
            Self::Ordinary(frames) => frames.pop(),
            Self::Controlled(frames) => frames.pop(),
        }
    }
}

impl Encoder<'_, '_> {
    pub(super) fn value(&mut self, value: &Value, ty: &ColumnType) -> Result<(), SQLError> {
        let mut stack = self.control.budget().map_or_else(
            || Stack::Ordinary(Vec::new()),
            |budget| Stack::Controlled(BudgetedVec::new(budget)),
        );
        let mut frame = Frame::Value(Input::Borrowed(value, ty));
        loop {
            self.control.check()?;
            self.frame(frame, &mut stack)?;
            let Some(next) = stack.pop() else { break };
            frame = next;
        }
        Ok(())
    }

    fn frame<'a>(&mut self, frame: Frame<'a>, stack: &mut Stack<'a>) -> Result<(), SQLError> {
        match frame {
            Frame::Value(input) => self.input(input, stack),
            Frame::Field(field, oid) => {
                let ty = field.declared_scalar_type();
                if oid {
                    self.write(
                        &ty.map_or(705, |ty| {
                            crate::catalog::type_metadata::pg_type_oid(ty) as u32
                        })
                        .to_be_bytes(),
                    )?;
                }
                if matches!(field.scalar_value(), Some(Value::Null)) {
                    return self.write(&(-1_i32).to_be_bytes());
                }
                validate_type(ty.unwrap_or(&ColumnType::Text), &self.control)?;
                let start = self.bytes.len();
                self.write(&[0; 4])?;
                stack.push(Frame::Finish(start))?;
                stack.push(Frame::Value(Input::Owned(field)))
            }
            Frame::Finish(start) => {
                let length =
                    i32::try_from(self.bytes.len() - start - 4).map_err(|_| super::size_error())?;
                self.bytes.as_mut_slice()[start..start + 4].copy_from_slice(&length.to_be_bytes());
                Ok(())
            }
            Frame::Record { input, index } => self.record_field(input, index, stack),
            Frame::Array {
                input,
                element,
                index,
                null_position,
            } => self.array_field(input, element, index, null_position, stack),
            Frame::Range {
                range,
                element,
                index,
            } => self.range_field(range, element, index, stack),
        }
    }

    fn input<'a>(&mut self, input: Input<'a>, stack: &mut Stack<'a>) -> Result<(), SQLError> {
        let (value, declared) = input.parts();
        validate_type(declared, &self.control)?;
        let mut ty = declared;
        while let ColumnType::Domain { base, .. } = ty {
            self.control.check()?;
            ty = base;
        }
        match ty {
            ColumnType::Record | ColumnType::Composite(_) => {
                let record =
                    crate::expr::SQLRecordView::new(value, Some(ty), self.catalog, &self.control)?;
                self.length(record.len())?;
                stack.push(Frame::Record { input, index: 0 })
            }
            ColumnType::Range(_)
            | ColumnType::Multirange(_)
            | ColumnType::DeclaredRange(_)
            | ColumnType::DeclaredMultirange(_) => self.range(value, ty, stack),
            _ if matches!(value, Value::Datum(_)) => {
                let Value::Datum(datum) = value else {
                    unreachable!()
                };
                let value = crate::expr::datums::read_with_value_catalog_and_control(
                    datum,
                    self.catalog,
                    &self.control,
                )?;
                let field = parameter(value, ty.clone_with_control(&self.control)?, &self.control)?;
                stack.push(Frame::Value(Input::Owned(field)))
            }
            ColumnType::Array(element) => {
                let (element, null_position) = self.array_header(value, element)?;
                stack.push(Frame::Array {
                    input,
                    element,
                    index: 0,
                    null_position,
                })
            }
            ColumnType::Int2Vector | ColumnType::OidVector | ColumnType::AnyArray => {
                let element = match ty {
                    ColumnType::Int2Vector => ColumnType::SmallInteger,
                    ColumnType::OidVector => ColumnType::Oid,
                    _ => ColumnType::Text,
                };
                let (element, null_position) = self.array_header(value, &element)?;
                stack.push(Frame::Array {
                    input,
                    element,
                    index: 0,
                    null_position,
                })
            }
            _ => self.decoded_value(value, ty),
        }
    }
}

pub(super) fn parameter(
    value: Produced<Value>,
    ty: Produced<ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<SQLParam>, SQLError> {
    let (value, value_memory) = value.into_parts();
    let (ty, type_memory) = ty.into_parts();
    Ok(control.finish(
        SQLParam::typed_scalar(value, ty),
        control.combine(value_memory, type_memory),
    )?)
}
