//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent type declarations retain their own decoded names and recursive payloads.

use super::ColumnType;
use crate::SQLError;
use uqa_core::{
    json::JsonToken,
    memory::{Produced, ProductionControl},
};

mod fields;
mod json;
mod references;
mod scalar;

use json::{invalid, number, optional, Input};

impl ColumnType {
    /// Read the existing stored JSON representation, admitting every temporary and output allocation before construction. This decodes the declaration; catalog identity and type validation remain with their existing owners.
    pub fn from_stored_json_with_control(
        text: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let mut input = Input::new(text, *control);
        let token = input.next()?;
        let value = input.column(token)?;
        input.end()?;
        Ok(value)
    }
}

impl<'a> Input<'a, '_> {
    fn column(&mut self, first: JsonToken<'a>) -> Result<Produced<ColumnType>, SQLError> {
        let (tag, object) = self.tag(first)?;
        let value = if let Some(value) = scalar::column(&tag) {
            if object && self.next()? != JsonToken::Null {
                return Err(invalid());
            }
            self.finish(value)?
        } else {
            if !object {
                return Err(invalid());
            }
            let token = self.next()?;
            self.payload(&tag, token)?
        };
        self.close_tag(object)?;
        Ok(value)
    }

    fn payload(
        &mut self,
        tag: &str,
        token: JsonToken<'a>,
    ) -> Result<Produced<ColumnType>, SQLError> {
        let scalar = match tag {
            "Named" => {
                let (name, memory) = self.text(token)?.into_parts();
                return Ok(self.control.finish(ColumnType::Named(name), memory)?);
            }
            "Array" => {
                let element = self.column(token)?;
                return Ok(ColumnType::array_with_control(element, &self.control)?);
            }
            "Domain" => return self.domain(token),
            "Enum" | "Composite" | "DeclaredRange" | "DeclaredMultirange" => {
                return self.reference(tag, token);
            }
            "Numeric" => self.numeric(token)?,
            "IntervalWithFields" => self.interval(token)?,
            "Varchar" => ColumnType::Varchar(optional(token)?),
            "Bit" => ColumnType::Bit(optional(token)?),
            "VarBit" => ColumnType::VarBit(optional(token)?),
            "Character" => ColumnType::Character(number(token)?),
            "TimePrecision" => ColumnType::TimePrecision(number(token)?),
            "TimeTzPrecision" => ColumnType::TimeTzPrecision(number(token)?),
            "TimestampPrecision" => ColumnType::TimestampPrecision(number(token)?),
            "TimestampTzPrecision" => ColumnType::TimestampTzPrecision(number(token)?),
            "Vector" => ColumnType::Vector(number(token)?),
            "Tensor" => ColumnType::Tensor(number(token)?),
            "Range" => ColumnType::Range(scalar::range(&self.unit_tag(token)?)?),
            "Multirange" => ColumnType::Multirange(scalar::range(&self.unit_tag(token)?)?),
            _ => return Err(invalid()),
        };
        self.finish(scalar)
    }

    fn finish(&self, value: ColumnType) -> Result<Produced<ColumnType>, SQLError> {
        Ok(self
            .control
            .finish(value, self.control.empty_reservation())?)
    }
}

#[cfg(test)]
mod tests;
