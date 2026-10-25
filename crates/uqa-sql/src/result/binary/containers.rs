//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected container fields advance one at a time through the admitted output stack.

use super::{
    carrier_error,
    traversal::{parameter, Frame, Input, Stack},
    validate_type, ColumnType, Encoder, SQLError, Value,
};
use crate::expr::{RangeComparisonValue, RangeValueDescriptor, SQLRecordView};
use uqa_core::memory::Produced;

impl Encoder<'_, '_> {
    pub(super) fn array_header(
        &mut self,
        value: &Value,
        element: &ColumnType,
    ) -> Result<(Produced<ColumnType>, usize), SQLError> {
        let array = value.array_view().ok_or_else(carrier_error)?;
        let mut element = element;
        while let ColumnType::Array(inner) = element {
            self.control.check()?;
            element = inner;
        }
        let element = if let Some(oid) = array
            .element_type_oid()
            .filter(|oid| crate::catalog::type_metadata::pg_type_oid(element) != i64::from(*oid))
        {
            crate::expr::type_by_oid_with_control(oid, self.catalog, &self.control)?
        } else {
            element.clone_with_control(&self.control)?
        };
        validate_type(&element, &self.control)?;
        self.length(array.dimensions().len())?;
        let null_position = self.bytes.len();
        self.write(&[0; 4])?;
        self.write(&(crate::catalog::type_metadata::pg_type_oid(&element) as u32).to_be_bytes())?;
        for (length, lower) in array.dimensions().iter().zip(array.lower_bounds()) {
            self.length(*length)?;
            self.write(&lower.to_be_bytes())?;
        }
        Ok((element, null_position))
    }

    pub(super) fn array_field<'a>(
        &mut self,
        input: Input<'a>,
        element: Produced<ColumnType>,
        index: usize,
        null_position: usize,
        stack: &mut Stack<'a>,
    ) -> Result<(), SQLError> {
        let array = input.parts().0.array_view().ok_or_else(carrier_error)?;
        let Some(value) = array.element_at_row_major_with_control(index, &self.control)? else {
            return Ok(());
        };
        if matches!(value, Value::Null) {
            self.bytes.as_mut_slice()[null_position + 3] = 1;
        }
        let field = parameter(
            self.control.copy_value(value)?,
            element.clone_with_control(&self.control)?,
            &self.control,
        )?;
        stack.push(Frame::Array {
            input,
            element,
            index: index + 1,
            null_position,
        })?;
        stack.push(Frame::Field(field, false))
    }

    pub(super) fn record_field<'a>(
        &mut self,
        input: Input<'a>,
        index: usize,
        stack: &mut Stack<'a>,
    ) -> Result<(), SQLError> {
        let (value, ty) = input.parts();
        let record = SQLRecordView::new(value, Some(ty), self.catalog, &self.control)?;
        let Some(field) = record.field_with_control(index, self.catalog, &self.control)? else {
            return Ok(());
        };
        stack.push(Frame::Record {
            input,
            index: index + 1,
        })?;
        stack.push(Frame::Field(field, true))
    }

    pub(super) fn range(
        &mut self,
        value: &Value,
        ty: &ColumnType,
        stack: &mut Stack<'_>,
    ) -> Result<(), SQLError> {
        let descriptor =
            RangeValueDescriptor::from_type_with_control(ty, self.catalog, &self.control)?
                .ok_or_else(carrier_error)?;
        let range =
            RangeComparisonValue::from_type_with_control(value, ty, self.catalog, &self.control)?
                .ok_or_else(carrier_error)?;
        let element = if let Some(bounds) = range.bounds() {
            let flags = if bounds.empty {
                1
            } else {
                (u8::from(bounds.lower_inclusive) << 1)
                    | (u8::from(bounds.upper_inclusive) << 2)
                    | (u8::from(bounds.lower.is_none()) << 3)
                    | (u8::from(bounds.upper.is_none()) << 4)
            };
            self.write(&[flags])?;
            if bounds.empty {
                return Ok(());
            }
            let (descriptor, memory) = descriptor.into_parts();
            self.control.finish(descriptor.endpoint, memory)?
        } else {
            self.length(range.member_count().ok_or_else(carrier_error)?)?;
            descriptor.range_type_with_control(self.catalog, &self.control)?
        };
        stack.push(Frame::Range {
            range,
            element,
            index: 0,
        })
    }

    pub(super) fn range_field(
        &mut self,
        range: RangeComparisonValue,
        element: Produced<ColumnType>,
        index: usize,
        stack: &mut Stack<'_>,
    ) -> Result<(), SQLError> {
        let value = if let Some(bounds) = range.bounds() {
            let Some(value) = bounds.lower.into_iter().chain(bounds.upper).nth(index) else {
                return Ok(());
            };
            self.control.copy_value(value)?
        } else {
            let Some(value) =
                range.member_value_with_control(index, self.catalog, &self.control)?
            else {
                return Ok(());
            };
            value
        };
        let field = parameter(
            value,
            element.clone_with_control(&self.control)?,
            &self.control,
        )?;
        stack.push(Frame::Range {
            range,
            element,
            index: index + 1,
        })?;
        stack.push(Frame::Field(field, false))
    }
}
