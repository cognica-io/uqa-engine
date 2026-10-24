//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Packed host tensors retain a validated dimension independently of their row count.

use super::{vector_value, Produced, ProductionControl, ProductionVec, SQLError, SQLParam, Value};

#[derive(Debug, Clone)]
pub struct SQLTensor {
    values: Vec<f32>,
    dimensions: u32,
}

impl SQLTensor {
    pub fn dimensions(&self) -> u32 {
        self.dimensions
    }

    pub fn row_count(&self) -> usize {
        self.values.len() / self.dimensions as usize
    }

    pub fn values(&self) -> &[f32] {
        &self.values
    }

    /// Validate shape and finite float32 elements before retaining a copied row-major input. An empty tensor still has a positive declared dimension.
    pub fn from_slice_with_control(
        values: &[f32],
        dimensions: u32,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        control.check()?;
        Self::validate_dimension(dimensions)?;
        if !values.len().is_multiple_of(dimensions as usize) {
            return Err(SQLError::TypeMismatch(
                "tensor element count must be a multiple of its dimension".into(),
            ));
        }
        for value in values {
            crate::expr::vector_element_with_control(&Value::Float(f64::from(*value)), control)?;
        }
        let mut output = ProductionVec::new(*control);
        output.extend_from_slice(values)?;
        let (values, memory) = output.finish()?.into_parts();
        Ok(control.finish(Self { values, dimensions }, memory)?)
    }

    /// Observe a selected SQL tensor using the existing native numeric conversion, checking every row against its declared dimension under the caller's allowance.
    pub fn from_value_with_control(
        value: &Value,
        dimensions: u32,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        control.check()?;
        Self::validate_dimension(dimensions)?;
        let rows = crate::expr::tensor_items(value)?;
        let count = rows
            .len()
            .checked_mul(dimensions as usize)
            .ok_or_else(|| SQLError::from(uqa_core::memory::MemoryError::SizeOverflow))?;
        let mut output = ProductionVec::new(*control);
        output.reserve(count)?;
        for row in rows {
            control.check()?;
            let values = crate::expr::vector_items(row)?;
            if values.len() != dimensions as usize {
                return Err(SQLError::TypeMismatch(format!(
                    "tensor row has {} elements, expected {dimensions}",
                    values.len()
                )));
            }
            for value in values {
                output.push_copy(crate::expr::vector_element_with_control(value, control)?)?;
            }
        }
        let (values, memory) = output.finish()?.into_parts();
        Ok(control.finish(Self { values, dimensions }, memory)?)
    }

    pub fn copy_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        control.check()?;
        let mut copy = ProductionVec::new(*control);
        copy.extend_from_slice(&self.values)?;
        let (values, memory) = copy.finish()?.into_parts();
        Ok(control.finish(
            Self {
                values,
                dimensions: self.dimensions,
            },
            memory,
        )?)
    }

    pub(super) fn to_value_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        let mut output = ProductionVec::new(*control);
        output.reserve(self.row_count())?;
        for row in self.values.chunks_exact(self.dimensions as usize) {
            output.push_produced(vector_value(row, control)?)?;
        }
        let (rows, memory) = output.finish()?.into_parts();
        Ok(control.finish(Value::List(rows), memory)?)
    }

    fn validate_dimension(dimensions: u32) -> Result<(), SQLError> {
        if dimensions == 0 {
            Err(SQLError::TypeMismatch(
                "tensor dimension must be a positive 32-bit integer".into(),
            ))
        } else {
            Ok(())
        }
    }
}

impl SQLParam {
    /// Construct a typed tensor from packed row-major floats, retaining its dimension even when no rows are present.
    pub fn tensor_with_control(
        values: &[f32],
        dimensions: u32,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let (tensor, memory) =
            SQLTensor::from_slice_with_control(values, dimensions, control)?.into_parts();
        Ok(control.finish(Self::TypedTensor(tensor), memory)?)
    }
}

#[cfg(test)]
mod tests;
