//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed packed vectors and tensors preserve SQL dimensions and admitted float storage.

use super::{boundary, ScalarAccess, UQAError, UQAValue, ValueSource};
use crate::abi::{UQATensorView, UQAVectorView, UQA_ERROR_RANGE, UQA_ERROR_TYPE, UQA_OK};
use uqa_core::memory::ProductionControl;
use uqa_sql::{params::SQLTensor, ColumnType, SQLParam};

fn dimensions(value: &UQAValue) -> boundary::Result<u32> {
    let mut ty = value.ty();
    while let Some(ColumnType::Domain { base, .. }) = ty {
        ty = Some(base);
    }
    match ty {
        Some(ColumnType::Vector(dimensions) | ColumnType::Tensor(dimensions)) => Ok(*dimensions),
        _ => Err(UQAError::new(
            UQA_ERROR_TYPE,
            "value has no vector or tensor descriptor",
        )),
    }
}

ffi_fn!(uqa_value_create_tensor_f32(data: *const f32, row_count: u64, dimensions: u32, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    let count = usize::try_from(row_count).ok().and_then(|rows| rows.checked_mul(dimensions as usize))
        .ok_or_else(|| UQAError::new(UQA_ERROR_RANGE, "tensor shape exceeds the addressable range"))?;
    let values = boundary::span(data, count)?;
    let parameter = SQLParam::tensor_with_control(values, dimensions, &ProductionControl::uncontrolled())?;
    *out = Box::into_raw(UQAValue::owned(parameter, None)?);
    Ok(UQA_OK)
});

ffi_fn!(uqa_value_get_vector_f32(value: *const UQAValue, out: *mut UQAVectorView) {
    let out = boundary::output(out)?;
    let value = boundary::input(value)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    value.check_access(ScalarAccess::Vector)?;
    let dimensions = dimensions(value)?;
    let values = if let ValueSource::Owned(parameter) = &value.source {
        if let SQLParam::Vector(values) = &**parameter { Some(values.as_slice()) } else { None }
    } else { None };
    let values = if let Some(values) = values { values } else {
        if value.vector_data.get().is_none() {
            let decoded = uqa_sql::expr::value_to_vector_with_control(value.observed()?, &value.control())?;
            if decoded.len() != dimensions as usize {
                return Err(UQAError::new(UQA_ERROR_RANGE, "vector carrier does not match its declared dimension"));
            }
            let _ = value.vector_data.set(decoded);
        }
        value.vector_data.get().ok_or_else(|| UQAError::state("vector decoder returned no data"))?.as_slice()
    };
    *out = UQAVectorView { data: values.as_ptr(), dimensions };
    Ok(UQA_OK)
});

ffi_fn!(uqa_value_get_tensor_f32(value: *const UQAValue, out: *mut UQATensorView) {
    let out = boundary::output(out)?;
    let value = boundary::input(value)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    value.check_access(ScalarAccess::Tensor)?;
    let dimensions = dimensions(value)?;
    let tensor = if let ValueSource::Owned(parameter) = &value.source {
        if let SQLParam::TypedTensor(tensor) = &**parameter { Some(tensor) } else { None }
    } else { None };
    let tensor = if let Some(tensor) = tensor { tensor } else {
        if value.tensor_data.get().is_none() {
            let decoded = SQLTensor::from_value_with_control(value.observed()?, dimensions, &value.control())?;
            let _ = value.tensor_data.set(decoded);
        }
        value.tensor_data.get().ok_or_else(|| UQAError::state("tensor decoder returned no data"))?
    };
    *out = UQATensorView {
        data: if tensor.values().is_empty() { std::ptr::null() } else { tensor.values().as_ptr() },
        row_count: tensor.row_count() as u64,
        dimensions,
    };
    Ok(UQA_OK)
});
