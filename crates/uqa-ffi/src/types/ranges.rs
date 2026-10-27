//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Range metadata borrows the original descriptor's session and production allowance.

use super::{boundary, UQAType};
use crate::abi::{UQA_DONE, UQA_OK};
use uqa_sql::{
    expr::{EngineValueCatalog, RangeValueDescriptor, SQLValueCatalog},
    ColumnType,
};

ffi_fn!(uqa_type_get_range_subtype(ty: *const UQAType, out: *mut *const UQAType) {
    let out = boundary::output(out)?;
    let ty = boundary::input(ty)?;
    let _call = ty.session.as_ref().map(|session| session.enter_value()).transpose()?;
    let catalog = ty.session.as_ref().map(|session| EngineValueCatalog(&session.native));
    let descriptor = RangeValueDescriptor::from_type_with_control(&ty.native, catalog.as_ref().map(|catalog| catalog as &dyn SQLValueCatalog), &ty.control())?;
    let Some(descriptor) = descriptor else { return Ok(UQA_DONE); };
    *out = ty.children.get(2, &descriptor.endpoint, ty.session.as_ref())?;
    Ok(UQA_OK)
});

ffi_fn!(uqa_type_get_range_type(ty: *const UQAType, out: *mut *const UQAType) {
    let out = boundary::output(out)?;
    let ty = boundary::input(ty)?;
    let _call = ty.session.as_ref().map(|session| session.enter_value()).transpose()?;
    ty.control().check()?;
    if !matches!(*ty.native, ColumnType::Multirange(_) | ColumnType::DeclaredMultirange(_)) { return Ok(UQA_DONE); }
    let catalog = ty.session.as_ref().map(|session| EngineValueCatalog(&session.native));
    let catalog = catalog.as_ref().map(|catalog| catalog as &dyn SQLValueCatalog);
    let Some(descriptor) = RangeValueDescriptor::from_type_with_control(&ty.native, catalog, &ty.control())? else { return Ok(UQA_DONE); };
    let range = descriptor.range_type_with_control(catalog, &ty.control())?;
    *out = ty.children.get(3, &range, ty.session.as_ref())?;
    Ok(UQA_OK)
});
