//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Range views retain admitted native bounds and only the member handles callers request.

use super::{arrays::Children, boundary, UQAError, UQAValue};
use crate::abi::{
    UQARangeBounds, UQA_ERROR_NULL, UQA_ERROR_RANGE, UQA_ERROR_TYPE, UQA_OK, UQA_RANGE_EMPTY,
    UQA_RANGE_LOWER_INCLUSIVE, UQA_RANGE_LOWER_UNBOUNDED, UQA_RANGE_UPPER_INCLUSIVE,
    UQA_RANGE_UPPER_UNBOUNDED,
};
use parking_lot::Mutex;
use std::sync::OnceLock;
use uqa_core::{
    memory::{MemoryReservation, Produced},
    Value,
};
use uqa_sql::{
    expr::{EngineValueCatalog, RangeComparisonValue, RangeValueDescriptor, SQLValueCatalog},
    ColumnType, SQLParam,
};

pub(super) struct RangeViews {
    native: RangeComparisonValue,
    descriptor: Produced<RangeValueDescriptor>,
    range_type: Produced<ColumnType>,
    bounds: OnceLock<BoundViews>,
    members: Mutex<Children>,
    _memory: Option<MemoryReservation>,
}

struct BoundViews {
    lower: Option<Box<UQAValue>>,
    upper: Option<Box<UQAValue>>,
    flags: u32,
}

impl BoundViews {
    fn view(&self) -> UQARangeBounds {
        UQARangeBounds {
            lower: self
                .lower
                .as_deref()
                .map_or(std::ptr::null(), std::ptr::from_ref),
            upper: self
                .upper
                .as_deref()
                .map_or(std::ptr::null(), std::ptr::from_ref),
            flags: self.flags,
        }
    }
}

fn observed(value: &UQAValue, multirange: bool) -> boundary::Result<&RangeViews> {
    let control = value.control();
    control.check()?;
    if matches!(value.raw(), Some(Value::Null)) {
        return Err(UQAError::new(
            UQA_ERROR_NULL,
            "SQL NULL has no range bounds or members",
        ));
    }
    let mut ty = value.ty().ok_or_else(wrong_type)?;
    while let ColumnType::Domain { base, .. } = ty {
        ty = base;
    }
    if if multirange {
        !matches!(
            ty,
            ColumnType::Multirange(_) | ColumnType::DeclaredMultirange(_)
        )
    } else {
        !matches!(ty, ColumnType::Range(_) | ColumnType::DeclaredRange(_))
    } {
        return Err(wrong_type());
    }
    let catalog = value
        .session
        .as_ref()
        .map(|session| EngineValueCatalog(&session.native));
    let catalog = catalog
        .as_ref()
        .map(|catalog| catalog as &dyn SQLValueCatalog);
    // Resolve the live declaration even when previously admitted views are already cached.
    let descriptor = RangeValueDescriptor::from_type_with_control(ty, catalog, &control)?
        .ok_or_else(wrong_type)?;
    if value.ranges.get().is_none() {
        let memory = control.reserve(size_of::<RangeViews>())?;
        let native = RangeComparisonValue::from_type_with_control(
            value.raw().ok_or_else(wrong_type)?,
            ty,
            catalog,
            &control,
        )?
        .ok_or_else(wrong_type)?;
        let range_type = descriptor.range_type_with_control(catalog, &control)?;
        let views = boundary::boxed(RangeViews {
            native,
            descriptor,
            range_type,
            bounds: OnceLock::new(),
            members: Mutex::new(Children::new(&control)),
            _memory: memory,
        })?;
        let _ = value.ranges.set(views);
    }
    value
        .ranges
        .get()
        .map(Box::as_ref)
        .ok_or_else(|| UQAError::state("range observation returned no value"))
}

fn wrong_type() -> UQAError {
    UQAError::new(
        UQA_ERROR_TYPE,
        "SQL type does not support the requested range accessor",
    )
}

ffi_fn!(uqa_value_get_range_bounds(value: *const UQAValue, out: *mut UQARangeBounds) {
    let out = boundary::output(out)?;
    let value = boundary::input(value)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    let ranges = observed(value, false)?;
    if ranges.bounds.get().is_none() {
        let native = ranges.native.bounds().ok_or_else(wrong_type)?;
        let child = |bound| UQAValue::borrowed_with_session(bound, Some(&ranges.descriptor.endpoint), value.session.as_ref());
        let lower = native.lower.map(child).transpose()?;
        let upper = native.upper.map(child).transpose()?;
        let flags = if native.empty { UQA_RANGE_EMPTY } else {
            (if lower.is_none() { UQA_RANGE_LOWER_UNBOUNDED } else if native.lower_inclusive { UQA_RANGE_LOWER_INCLUSIVE } else { 0 })
            | (if upper.is_none() { UQA_RANGE_UPPER_UNBOUNDED } else if native.upper_inclusive { UQA_RANGE_UPPER_INCLUSIVE } else { 0 })
        };
        let _ = ranges.bounds.set(BoundViews { lower, upper, flags });
    }
    *out = ranges.bounds.get().ok_or_else(|| UQAError::state("range has no bound views"))?.view();
    Ok(UQA_OK)
});

ffi_fn!(uqa_value_get_range_count(value: *const UQAValue, out: *mut u32) {
    let out = boundary::output(out)?;
    let value = boundary::input(value)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    let ranges = observed(value, true)?;
    *out = u32::try_from(ranges.native.member_count().ok_or_else(wrong_type)?).map_err(|_| UQAError::new(UQA_ERROR_RANGE, "multirange member count exceeds the C range"))?;
    Ok(UQA_OK)
});

ffi_fn!(uqa_value_get_range(value: *const UQAValue, index: u32, out: *mut *const UQAValue) {
    let out = boundary::output(out)?;
    let value = boundary::input(value)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    let ranges = observed(value, true)?;
    if index as usize >= ranges.native.member_count().ok_or_else(wrong_type)? { return Err(UQAError::new(UQA_ERROR_RANGE, "multirange member index is out of range")); }
    let control = value.control();
    *out = ranges.members.lock().get_or_create(u64::from(index), &control, || {
        let catalog = value.session.as_ref().map(|session| EngineValueCatalog(&session.native));
        let native = ranges.native.member_value_with_control(index as usize, catalog.as_ref().map(|catalog| catalog as &dyn SQLValueCatalog), &control)?
            .ok_or_else(|| UQAError::state("multirange has no requested member"))?;
        let (native, memory) = native.into_parts();
        let (ty, type_memory) = ranges.range_type.clone_with_control(&control)?.into_parts();
        let parameter = control.finish(SQLParam::typed_scalar(native, ty), control.combine(memory, type_memory))?;
        UQAValue::owned(parameter, value.session.clone())
    })?;
    Ok(UQA_OK)
});
