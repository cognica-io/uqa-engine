//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Observe current composite attributes while retaining previously returned field views.

use super::{boundary, UQAError, UQAType};
use crate::abi::{UQAFieldDescriptor, UQA_DONE, UQA_ERROR_RANGE, UQA_OK};
use parking_lot::Mutex;
use std::sync::Arc;
use uqa_core::memory::{MemoryReservation, OwnedMap, Produced, ProductionControl};
use uqa_sql::{
    expr::{composites::CompositeTypeDescriptor, EngineHook},
    ColumnType,
};

pub(super) struct FieldViews(Mutex<Fields>);

struct Fields {
    values: OwnedMap<usize, Box<Field>>,
    memory: Option<MemoryReservation>,
}

impl FieldViews {
    pub(super) fn new(control: &ProductionControl<'_>) -> Self {
        Self(Mutex::new(Fields {
            values: OwnedMap::new(),
            memory: control.empty_reservation(),
        }))
    }
}

struct Field {
    name: Produced<String>,
    ty: Box<UQAType>,
    number: i32,
    collation_oid: u32,
    _memory: Option<MemoryReservation>,
}

impl Field {
    fn view(&self) -> UQAFieldDescriptor {
        UQAFieldDescriptor {
            name: self.name.as_str().into(),
            ty: std::ptr::from_ref(&*self.ty),
            number: self.number,
            collation_oid: self.collation_oid,
        }
    }
}

fn descriptor(ty: &UQAType) -> boundary::Result<Option<Arc<CompositeTypeDescriptor>>> {
    ty.control().check()?;
    let ColumnType::Composite(reference) = &*ty.native else {
        return Ok(None);
    };
    let catalog = ty.session.as_ref().map(|session| &session.native);
    Ok(Some(uqa_sql::expr::composites::descriptor(
        catalog.and_then(|engine| engine.composite_types()),
        reference.oid,
    )?))
}

ffi_fn!(uqa_type_get_field_count(ty: *const UQAType, out: *mut u32) {
    let out = boundary::output(out)?;
    let ty = boundary::input(ty)?;
    let _call = ty.session.as_ref().map(|session| session.enter_value()).transpose()?;
    let Some(descriptor) = descriptor(ty)? else { return Ok(UQA_DONE); };
    *out = u32::try_from(descriptor.attributes.len()).map_err(|_| UQAError::new(UQA_ERROR_RANGE, "composite field count exceeds the C range"))?;
    Ok(UQA_OK)
});

ffi_fn!(uqa_type_get_field(ty: *const UQAType, index: u32, out: *mut UQAFieldDescriptor) {
    let out = boundary::output(out)?;
    let ty = boundary::input(ty)?;
    let _call = ty.session.as_ref().map(|session| session.enter_value()).transpose()?;
    let Some(descriptor) = descriptor(ty)? else { return Ok(UQA_DONE); };
    let attribute = descriptor.attributes.get(index as usize).ok_or_else(|| UQAError::new(UQA_ERROR_RANGE, "composite field index is out of range"))?;
    let control = ty.control();
    let mut fields = ty.fields.0.lock();
    for (_, field) in &fields.values {
        control.check()?;
        if *field.name == attribute.name && *field.ty.native == attribute.ty
            && field.number == i32::from(attribute.number) && field.collation_oid == attribute.collation_oid() {
            *out = field.view();
            return Ok(UQA_OK);
        }
    }
    let node = control.reserve(OwnedMap::<usize, Box<Field>>::entry_bytes())?;
    let memory = control.reserve(size_of::<Field>())?;
    let field = boundary::boxed(Field {
        name: control.copy_text(&attribute.name)?,
        ty: UQAType::new(&attribute.ty, ty.session.as_ref())?,
        number: i32::from(attribute.number),
        collation_oid: attribute.collation_oid(),
        _memory: memory,
    })?;
    let view = field.view();
    let key = fields.values.len();
    fields.values.insert(key, field);
    fields.memory = control.combine(fields.memory.take(), node);
    *out = view;
    Ok(UQA_OK)
});
