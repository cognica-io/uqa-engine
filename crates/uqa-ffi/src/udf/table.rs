//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One admitted row per pull, with native fallible scan finalization.
#![allow(unsafe_code)]

use super::{
    registration::{options, registration_error, SharedData},
    resource::Resource,
    support::{self, sql_error, Arguments},
    UQAUDFOptions, UQAUserDataDestroyFn, UQAUDF,
};
use crate::{
    abi::{UQAStatus, UQA_DONE, UQA_OK},
    boundary,
    callback::{self, Callbacks},
    error::{RetainedError, UQAError},
    execute,
    session::{Session, UQAEngine},
    value::UQAValue,
};
use std::{
    ffi::{c_char, c_void},
    sync::Arc,
};
use uqa_core::{
    memory::{Produced, ProductionVec},
    Value,
};
use uqa_engine::{SQLFunctionContext, SQLRowSource, SQLTableFunction, SQLTableFunctionStream};
use uqa_sql::SQLError;

pub type UQAUDFCreateScanFn =
    unsafe extern "C" fn(*const *const UQAValue, u32, *mut c_void, *mut *mut c_void) -> UQAStatus;
pub type UQAUDFNextFn =
    unsafe extern "C" fn(*mut UQAUDFRowSink, *mut c_void, *mut c_void) -> UQAStatus;
pub type UQAUDFDestroyScanFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> UQAStatus;

struct Table {
    columns: Produced<Vec<String>>,
    create: UQAUDFCreateScanFn,
    next: UQAUDFNextFn,
    destroy: UQAUDFDestroyScanFn,
    data: SharedData,
}
struct Scan {
    resource: Resource,
    next: UQAUDFNextFn,
    width: usize,
}

pub struct UQAUDFRowSink {
    session: Arc<Session>,
    failure: RetainedError,
    width: usize,
    row: Option<Produced<Vec<Value>>>,
}

impl SQLTableFunction for Table {
    fn output_columns(&self) -> Option<&[String]> {
        Some(&self.columns)
    }

    fn call_stream_with_context(
        &self,
        native: &SQLFunctionContext,
        values: &[Value],
    ) -> Result<SQLTableFunctionStream, SQLError> {
        let session = support::session(native).map_err(sql_error)?;
        let arguments = Arguments::new(&session, values).map_err(sql_error)?;
        let control = session.control();
        let mut columns = ProductionVec::new(control);
        columns
            .reserve(self.columns.len())
            .map_err(SQLError::from)?;
        for column in self.columns.iter() {
            columns.push_produced(control.copy_text(column)?)?;
        }
        let columns = columns.finish()?;
        let mut scan = boundary::boxed(Scan {
            resource: Resource::new(&session, &self.data, self.destroy, size_of::<Scan>())
                .map_err(sql_error)?,
            next: self.next,
            width: columns.len(),
        })
        .map_err(sql_error)?;
        let callbacks = Callbacks::new(&session).map_err(sql_error)?;
        let mut state = std::ptr::null_mut();
        let status = callbacks.invoke(|| unsafe {
            (self.create)(
                arguments.pointer(),
                arguments.count(),
                self.data.pointer,
                &raw mut state,
            )
        });
        support::accept(&callbacks, status, false)?;
        scan.resource.created(state);
        // Execution adopts these existing column and row buffers at its native boundary.
        Ok(SQLTableFunctionStream::from_source(
            columns.into_parts().0,
            *scan,
        ))
    }
}

unsafe fn copy_columns(
    session: &Arc<Session>,
    columns: *const *const c_char,
    sizes: Option<*const usize>,
    count: u32,
) -> boundary::Result<Produced<Vec<String>>> {
    if count == 0 {
        return Err(UQAError::invalid(
            "table registration requires output columns",
        ));
    }
    let names = boundary::span(columns, count as usize)?;
    let sizes = sizes
        .map(|sizes| boundary::span(sizes, count as usize))
        .transpose()?;
    let control = session.control();
    let mut copied = ProductionVec::new(control);
    copied.reserve(names.len())?;
    for (index, name) in names.iter().enumerate() {
        let name = match sizes {
            Some(sizes) => boundary::required_text_n(*name, sizes[index])?,
            None => boundary::text(*name)?,
        };
        copied.push_produced(control.copy_text(name)?)?;
    }
    Ok(copied.finish()?)
}

impl SQLRowSource<Vec<Value>> for Scan {
    fn next_row(&mut self) -> Result<Option<Vec<Value>>, SQLError> {
        let state = self.resource.pointer()?;
        let session = self.resource.session()?;
        session.control().check().map_err(SQLError::from)?;
        let callbacks = Callbacks::new(&session).map_err(sql_error)?;
        let mut sink = UQAUDFRowSink {
            session: Arc::clone(&session),
            failure: Arc::clone(&callbacks.failure),
            width: self.width,
            row: None,
        };
        let pointer = &raw mut sink;
        let status =
            callbacks.invoke(|| unsafe { (self.next)(pointer, state, self.resource.data.pointer) });
        support::accept(&callbacks, status, true)?;
        match (status, sink.row) {
            (UQA_DONE, None) => Ok(None),
            (UQA_OK, Some(row)) => Ok(Some(row.into_parts().0)),
            (UQA_DONE, Some(_)) => Err(sql_error(UQAError::state(
                "table callback emitted a row with UQA_DONE",
            ))),
            _ => Err(sql_error(UQAError::state(
                "table callback returned UQA_OK without a row",
            ))),
        }
    }

    fn close(&mut self) -> Result<(), SQLError> {
        self.resource.close()
    }
}

ffi_fn!(uqa_udf_row_sink_emit(sink: *mut UQAUDFRowSink, values: *const *const UQAValue, count: u32) {
    let sink = boundary::input_mut(sink)?;
    callback::check_scope(&sink.failure)?;
    if sink.row.is_some() { return Err(UQAError::state("table callback already emitted a row")); }
    if count as usize != sink.width { return Err(UQAError::invalid("table callback row has the wrong column count")); }
    let values = boundary::span(values, count as usize)?;
    let control = sink.session.control();
    let mut row = ProductionVec::new(control);
    row.reserve(values.len())?;
    for pointer in values {
        let parameter = execute::parameter(&sink.session, boundary::input(*pointer)?)?;
        let value = UQAValue::owned(parameter, Some(Arc::clone(&sink.session)))?;
        row.push_produced((*value).into_native()?)?;
    }
    sink.row = Some(row.finish()?);
    Ok(UQA_OK)
});

macro_rules! table_registration {
    ($api:ident, $read:ident, $sizes:expr $(, $size:ident, $column_sizes:ident)?) => {
        ffi_fn!($api(engine: *mut UQAEngine, name: *const c_char, $($size: usize,)? options_pointer: *mut UQAUDFOptions, columns: *const *const c_char, $($column_sizes: *const usize,)? column_count: u32, create_scan: Option<UQAUDFCreateScanFn>, next: Option<UQAUDFNextFn>, destroy_scan: Option<UQAUDFDestroyScanFn>, user_data: *mut c_void, destroy_user_data: Option<UQAUserDataDestroyFn>, out: *mut *mut UQAUDF) {
            let out = boundary::new_handle(out)?;
            let name = boundary::$read(name $(, $size)?)?;
            let create = create_scan.ok_or_else(|| UQAError::invalid("table create_scan must not be NULL"))?;
            let next = next.ok_or_else(|| UQAError::invalid("table next must not be NULL"))?;
            let destroy = destroy_scan.ok_or_else(|| UQAError::invalid("table destroy_scan must not be NULL"))?;
            let options = options(options_pointer)?;
            let session = &boundary::input(engine)?.session;
            let _call = session.enter()?;
            let columns = copy_columns(session, columns, $sizes, column_count)?;
            let handle = UQAUDF::prepare(session, user_data, destroy_user_data)?;
            let native = session.native.register_table_function_retained(name, options, Table { columns, create, next, destroy, data: Arc::clone(&handle.data) }).map_err(registration_error)?;
            *out = handle.publish(native);
            Ok(UQA_OK)
        });
    };
}
table_registration!(uqa_udf_register_table, text, None);
table_registration!(
    uqa_udf_register_table_n,
    required_text_n,
    Some(column_sizes),
    name_size,
    column_sizes
);
