//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Record children retain their selected type and value for the parent's lifetime.

use super::{arrays, boundary, ScalarAccess, UQAError, UQAValue};
use crate::abi::{UQA_ERROR_RANGE, UQA_ERROR_TYPE, UQA_OK};
use uqa_core::{
    memory::{MemoryReservation, OwnedMap, Produced, ProductionControl},
    Value,
};
use uqa_sql::expr::{
    composites::CompositeTypeDescriptor, EngineValueCatalog, SQLRecordView, SQLValueCatalog,
};

pub(super) struct RecordViews {
    generations: OwnedMap<usize, Box<Generation>>,
    memory: Option<MemoryReservation>,
}

struct Generation {
    descriptor: Option<Produced<CompositeTypeDescriptor>>,
    children: arrays::Children,
    _memory: Option<MemoryReservation>,
}

impl RecordViews {
    pub(super) fn new(control: &ProductionControl<'_>) -> Self {
        Self {
            generations: OwnedMap::new(),
            memory: control.empty_reservation(),
        }
    }

    fn field(
        &mut self,
        parent: &UQAValue,
        view: &SQLRecordView<'_>,
        index: u32,
        catalog: Option<&dyn SQLValueCatalog>,
    ) -> boundary::Result<*const UQAValue> {
        let control = parent.control();
        let create = || {
            let parameter = view
                .field_with_control(index as usize, catalog, &control)?
                .ok_or_else(|| {
                    UQAError::new(UQA_ERROR_RANGE, "record field index is out of range")
                })?;
            UQAValue::owned(parameter, parent.session.clone())
        };
        let mut matching = None;
        for (key, generation) in &self.generations {
            control.check()?;
            if generation.descriptor.as_deref() == view.descriptor() {
                matching = Some(*key);
                break;
            }
        }
        if let Some(key) = matching {
            return self
                .generations
                .get_mut(&key)
                .expect("selected generation")
                .children
                .get_or_create(u64::from(index), &control, create);
        }
        let node = control.reserve(OwnedMap::<usize, Box<Generation>>::entry_bytes())?;
        let memory = control.reserve(size_of::<Generation>())?;
        let mut generation = boundary::boxed(Generation {
            descriptor: view
                .descriptor()
                .map(|descriptor| descriptor.clone_with_control(&control))
                .transpose()?,
            children: arrays::Children::new(&control),
            _memory: memory,
        })?;
        let field = generation
            .children
            .get_or_create(u64::from(index), &control, create)?;
        self.generations.insert(self.generations.len(), generation);
        self.memory = control.combine(self.memory.take(), node);
        Ok(field)
    }
}

fn record<'a>(
    value: &'a UQAValue,
    catalog: Option<&dyn SQLValueCatalog>,
) -> boundary::Result<SQLRecordView<'a>> {
    value.check_access(ScalarAccess::Record)?;
    let raw = value
        .raw()
        .ok_or_else(|| UQAError::new(UQA_ERROR_TYPE, "value is not a record"))?;
    if !matches!(raw, Value::Record(_) | Value::Row(_) | Value::Datum(_)) {
        return Err(UQAError::new(UQA_ERROR_TYPE, "value is not a record"));
    }
    if value.ty().is_none() && matches!(raw, Value::Datum(_)) {
        let inferred = uqa_sql::type_resolution::runtime_value_type_with_control(
            raw,
            catalog,
            &value.control(),
        )?;
        let mut ty = inferred.as_deref();
        while let Some(uqa_sql::ColumnType::Domain { base, .. }) = ty {
            ty = Some(base);
        }
        if !matches!(
            ty,
            Some(uqa_sql::ColumnType::Composite(_) | uqa_sql::ColumnType::Record)
        ) {
            return Err(UQAError::new(UQA_ERROR_TYPE, "value is not a record"));
        }
    }
    Ok(SQLRecordView::new(
        raw,
        value.ty(),
        catalog,
        &value.control(),
    )?)
}

ffi_fn!(uqa_value_get_field_count(value: *const UQAValue, out: *mut u32) {
    let out = boundary::output(out)?;
    let value = boundary::input(value)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    let catalog = value.session.as_ref().map(|session| EngineValueCatalog(&session.native));
    let view = record(value, catalog.as_ref().map(|catalog| catalog as &dyn SQLValueCatalog))?;
    *out = u32::try_from(view.len()).map_err(|_| UQAError::new(UQA_ERROR_RANGE, "record field count exceeds the C range"))?;
    Ok(UQA_OK)
});

ffi_fn!(uqa_value_get_field(value: *const UQAValue, index: u32, out: *mut *const UQAValue) {
    let out = boundary::output(out)?;
    let value = boundary::input(value)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    let catalog = value.session.as_ref().map(|session| EngineValueCatalog(&session.native));
    let catalog = catalog.as_ref().map(|catalog| catalog as &dyn SQLValueCatalog);
    let view = record(value, catalog)?;
    if index as usize >= view.len() {
        return Err(UQAError::new(UQA_ERROR_RANGE, "record field index is out of range"));
    }
    *out = value.record_views.lock().field(value, &view, index, catalog)?;
    Ok(UQA_OK)
});
