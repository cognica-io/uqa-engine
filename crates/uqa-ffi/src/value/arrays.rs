//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Array views borrow native row-major leaves and retain only requested wrappers and shape metadata.

use super::{boundary, ScalarAccess, UQAError, UQAValue};
use crate::abi::{UQAArrayDimension, UQAArrayShape, UQA_ERROR_RANGE, UQA_ERROR_TYPE, UQA_OK};
use uqa_core::{
    memory::{MemoryReservation, OwnedMap, ProductionControl, ProductionVec},
    ArrayValue,
};
use uqa_sql::ColumnType;

pub(super) struct Children {
    values: OwnedMap<u64, Box<UQAValue>>,
    memory: Option<MemoryReservation>,
}

impl Children {
    pub(super) fn new(control: &ProductionControl<'_>) -> Self {
        Self {
            values: OwnedMap::new(),
            memory: control.empty_reservation(),
        }
    }
}

fn array(value: &UQAValue) -> boundary::Result<&ArrayValue> {
    value.check_access(ScalarAccess::Array)?;
    value
        .observed()?
        .array_view()
        .ok_or_else(|| UQAError::new(UQA_ERROR_TYPE, "value is not a SQL array or catalog vector"))
}

fn element_type(value: &UQAValue) -> Option<&ColumnType> {
    let mut ty = value.ty()?;
    while let ColumnType::Domain { base, .. } = ty {
        ty = base;
    }
    match ty {
        ColumnType::Int2Vector => Some(&ColumnType::SmallInteger),
        ColumnType::OidVector => Some(&ColumnType::Oid),
        _ => ty.array_element_type(),
    }
}

ffi_fn!(uqa_value_get_array_shape(value: *const UQAValue, out: *mut UQAArrayShape) {
    let out = boundary::output(out)?;
    let value = boundary::input(value)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    let array = array(value)?;
    value.control().check()?;
    let rank = u32::try_from(array.dimensions().len()).map_err(|_| UQAError::new(UQA_ERROR_RANGE, "array rank exceeds the C range"))?;
    let mut count = u64::from(rank != 0);
    for length in array.dimensions() {
        value.control().check()?;
        count = count.checked_mul(*length as u64).ok_or_else(|| UQAError::new(UQA_ERROR_RANGE, "array element count exceeds the C range"))?;
    }
    if value.array_shape.get().is_none() {
        let mut dimensions = ProductionVec::new(value.control());
        dimensions.reserve(array.dimensions().len())?;
        for (length, lower_bound) in array.dimensions().iter().zip(array.lower_bounds()) {
            dimensions.push_copy(UQAArrayDimension { length: *length as u64, lower_bound: *lower_bound })?;
        }
        let _ = value.array_shape.set(dimensions.finish()?);
    }
    let dimensions = value.array_shape.get().ok_or_else(|| UQAError::state("array shape producer returned no dimensions"))?;
    *out = UQAArrayShape { dimensions: if rank == 0 { std::ptr::null() } else { dimensions.as_ptr() }, rank, element_count: count };
    Ok(UQA_OK)
});

ffi_fn!(uqa_value_get_element(value: *const UQAValue, index: u64, out: *mut *const UQAValue) {
    let out = boundary::output(out)?;
    let value = boundary::input(value)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    let array = array(value)?;
    let position = usize::try_from(index).map_err(|_| UQAError::new(UQA_ERROR_RANGE, "array element index exceeds the addressable range"))?;
    let element = array.element_at_row_major_with_control(position, &value.control())?
        .ok_or_else(|| UQAError::new(UQA_ERROR_RANGE, "array element index is out of range"))?;
    let mut children = value.array_children.lock();
    if let Some(child) = children.values.get(&index) { *out = std::ptr::from_ref(&**child); return Ok(UQA_OK); }
    let node = value.control().reserve(OwnedMap::<u64, Box<UQAValue>>::entry_bytes())?;
    let child = UQAValue::borrowed_with_session(element, element_type(value), value.session.as_ref())?;
    let pointer = std::ptr::from_ref(&*child);
    children.values.insert(index, child);
    children.memory = value.control().combine(children.memory.take(), node);
    *out = pointer;
    Ok(UQA_OK)
});
