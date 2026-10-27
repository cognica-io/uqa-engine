//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! C owners for session-native prepared identities and independently admitted bindings.

use parking_lot::Mutex;
use std::{ffi::c_char, sync::Arc};
use uqa_core::{
    memory::{BudgetedVec, MemoryReservation, Produced, ProductionVec},
    Value,
};
use uqa_engine::PreparedStatement;
use uqa_sql::{ColumnType, RowSchema, SQLParam};

use crate::{
    abi::{UQAStatus, UQAStringView, UQA_DONE, UQA_ERROR_RANGE, UQA_ERROR_STATE, UQA_OK},
    boundary,
    error::UQAError,
    execute::parameter,
    result::{NativeResult, ResultData, UQAResult},
    session::{Session, UQAEngine},
    types::{self, UQAType},
    value::UQAValue,
};

pub struct UQAStatement {
    native: Option<PreparedStatement>,
    parameter_types: BudgetedVec<Option<Produced<ColumnType>>>,
    result_schema: Option<RowSchema>,
    types: types::TypeViews,
    bindings: Mutex<Bindings>,
    session: Arc<Session>,
    _memory: MemoryReservation,
}

struct Bindings {
    values: BudgetedVec<SQLParam>,
    owners: BudgetedVec<BindingOwner>,
}

enum BindingOwner {
    Unbound,
    Bound { _memory: Option<MemoryReservation> },
}

impl UQAStatement {
    fn native(&self) -> &PreparedStatement {
        self.native
            .as_ref()
            .expect("published statement has a native identity")
    }

    fn validate(&self) -> boundary::Result<()> {
        Ok(self
            .session
            .native
            .validate_prepared_statement(self.native())?)
    }

    fn parameter_index(&self, index: u32) -> boundary::Result<usize> {
        let index = index.checked_sub(1).map(|index| index as usize);
        index
            .filter(|index| *index < self.parameter_types.len())
            .ok_or_else(|| UQAError::new(UQA_ERROR_RANGE, "parameter index is out of range"))
    }

    fn column(&self, index: u32) -> boundary::Result<(&RowSchema, usize)> {
        self.result_schema
            .as_ref()
            .filter(|schema| (index as usize) < schema.len())
            .map(|schema| (schema, index as usize))
            .ok_or_else(|| UQAError::new(UQA_ERROR_RANGE, "column index is out of range"))
    }
}

impl Drop for UQAStatement {
    fn drop(&mut self) {
        if let Some(native) = &self.native {
            self.session.native.finalize_prepared_statement(native);
        }
    }
}

// SQL and type-name spans obey the public argument lifetime contract.
#[allow(unsafe_code)]
unsafe fn prepare(
    engine: *mut UQAEngine,
    sql: &str,
    types: *const *const c_char,
    sizes: Option<*const usize>,
    count: u32,
    out: *mut *mut UQAStatement,
) -> boundary::Result<UQAStatus> {
    let out = boundary::new_handle(out)?;
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    let names = boundary::span(types, count as usize)?;
    let sizes = sizes
        .map(|sizes| boundary::span(sizes, count as usize))
        .transpose()?;
    let mut declared = ProductionVec::new(session.control());
    declared.reserve(names.len())?;
    for (index, name) in names.iter().enumerate() {
        let name = match sizes {
            Some(sizes) => boundary::text_n(*name, sizes[index])?,
            None => boundary::text(*name)?,
        };
        let control = session.control();
        let (name, memory) = control.copy_text(name)?.into_parts();
        declared.push_produced(control.finish(ColumnType::Named(name), memory)?)?;
    }
    let declared = declared.finish()?;
    let memory = session.memory.reserve(size_of::<UQAStatement>())?;
    let mut statement = boundary::boxed(UQAStatement {
        native: None,
        parameter_types: BudgetedVec::new(&session.memory),
        result_schema: None,
        types: types::TypeViews::new(&session.control()),
        bindings: Mutex::new(Bindings {
            values: BudgetedVec::new(&session.memory),
            owners: BudgetedVec::new(&session.memory),
        }),
        session: Arc::clone(session),
        _memory: memory,
    })?;
    statement.native = Some(session.execute(|native| native.prepare_statement(sql, &declared))?);
    // The owner removes the definition if any subsequent descriptor/binding admission fails.
    let description = session
        .native
        .describe_prepared_statement(statement.native())?;
    u32::try_from(description.parameter_types.len())
        .map_err(|_| UQAError::new(UQA_ERROR_RANGE, "parameter count exceeds the C range"))?;
    u32::try_from(description.result_schema.as_ref().map_or(0, RowSchema::len))
        .map_err(|_| UQAError::new(UQA_ERROR_RANGE, "column count exceeds the C range"))?;
    statement
        .parameter_types
        .reserve(description.parameter_types.len())?;
    let bindings = statement.bindings.get_mut();
    bindings.values.reserve(description.parameter_types.len())?;
    bindings.owners.reserve(description.parameter_types.len())?;
    for ty in description.parameter_types {
        statement.parameter_types.push(
            ty.as_ref()
                .map(|ty| ty.clone_with_control(&session.control()))
                .transpose()?,
        )?;
        bindings.values.push(SQLParam::Scalar(Value::Null))?;
        bindings.owners.push(BindingOwner::Unbound)?;
    }
    statement.result_schema = description.result_schema;
    *out = Box::into_raw(statement);
    Ok(UQA_OK)
}

ffi_fn!(uqa_statement_prepare(engine: *mut UQAEngine, sql: *const c_char, out: *mut *mut UQAStatement) {
    prepare(engine, boundary::text(sql)?, std::ptr::null(), None, 0, out)
});
ffi_fn!(uqa_statement_prepare_n(engine: *mut UQAEngine, sql: *const c_char, sql_size: usize, out: *mut *mut UQAStatement) {
    prepare(engine, boundary::text_n(sql, sql_size)?, std::ptr::null(), None, 0, out)
});
ffi_fn!(uqa_statement_prepare_typed(engine: *mut UQAEngine, sql: *const c_char, parameter_types: *const *const c_char, parameter_count: u32, out: *mut *mut UQAStatement) {
    prepare(engine, boundary::text(sql)?, parameter_types, None, parameter_count, out)
});
ffi_fn!(uqa_statement_prepare_typed_n(engine: *mut UQAEngine, sql: *const c_char, sql_size: usize, parameter_types: *const *const c_char, parameter_type_sizes: *const usize, parameter_count: u32, out: *mut *mut UQAStatement) {
    prepare(engine, boundary::text_n(sql, sql_size)?, parameter_types, Some(parameter_type_sizes), parameter_count, out)
});

ffi_fn!(uqa_statement_get_parameter_count(statement: *mut UQAStatement, out: *mut u32) {
    let out = boundary::output(out)?;
    let statement = boundary::input(statement)?;
    let _call = statement.session.enter()?;
    statement.validate()?;
    *out = statement.parameter_types.len() as u32;
    Ok(UQA_OK)
});
ffi_fn!(uqa_statement_get_parameter_type(statement: *mut UQAStatement, index: u32, out: *mut *const UQAType) {
    let out = boundary::output(out)?;
    let statement = boundary::input(statement)?;
    let _call = statement.session.enter()?;
    statement.validate()?;
    match statement.parameter_types[statement.parameter_index(index)?].as_deref() {
        Some(ty) => { *out = statement.types.get(u64::from(index), ty, Some(&statement.session))?; Ok(UQA_OK) },
        None => Ok(UQA_DONE),
    }
});
ffi_fn!(uqa_statement_get_column_count(statement: *mut UQAStatement, out: *mut u32) {
    let out = boundary::output(out)?;
    let statement = boundary::input(statement)?;
    let _call = statement.session.enter()?;
    statement.validate()?;
    *out = statement.result_schema.as_ref().map_or(0, RowSchema::len) as u32;
    Ok(UQA_OK)
});
ffi_fn!(uqa_statement_get_column_name(statement: *mut UQAStatement, column: u32, out: *mut UQAStringView) {
    let out = boundary::output(out)?;
    let statement = boundary::input(statement)?;
    let _call = statement.session.enter()?;
    statement.validate()?;
    let (schema, column) = statement.column(column)?;
    *out = schema.columns()[column].as_str().into();
    Ok(UQA_OK)
});
ffi_fn!(uqa_statement_get_column_type(statement: *mut UQAStatement, column: u32, out: *mut *const UQAType) {
    let out = boundary::output(out)?;
    let statement = boundary::input(statement)?;
    let _call = statement.session.enter()?;
    statement.validate()?;
    let (schema, column) = statement.column(column)?;
    match schema.column_type(column) {
        Some(ty) => { *out = statement.types.get((1 << 32) | column as u64, ty, Some(&statement.session))?; Ok(UQA_OK) },
        None => Ok(UQA_DONE),
    }
});
ffi_fn!(uqa_statement_bind(statement: *mut UQAStatement, index: u32, value: *const UQAValue) {
    let statement = boundary::input(statement)?;
    let _call = statement.session.enter()?;
    statement.validate()?;
    let index = statement.parameter_index(index)?;
    let replacement = parameter(&statement.session, boundary::input(value)?)?;
    let (value, memory) = replacement.into_parts();
    let mut bindings = statement.bindings.lock();
    bindings.values[index] = value;
    bindings.owners[index] = BindingOwner::Bound { _memory: memory };
    Ok(UQA_OK)
});
ffi_fn!(uqa_statement_clear_bindings(statement: *mut UQAStatement) {
    let statement = boundary::input(statement)?;
    let _call = statement.session.enter()?;
    statement.validate()?;
    let mut bindings = statement.bindings.lock();
    for index in 0..bindings.values.len() {
        bindings.values[index] = SQLParam::Scalar(Value::Null);
        bindings.owners[index] = BindingOwner::Unbound;
    }
    Ok(UQA_OK)
});
ffi_fn!(uqa_statement_execute(statement: *mut UQAStatement, result: *mut UQAResult) {
    boundary::input(result)?;
    let statement = boundary::input(statement)?;
    let _call = statement.session.enter()?;
    statement.validate()?;
    let bindings = statement.bindings.lock();
    if bindings.owners.iter().any(|owner| matches!(owner, BindingOwner::Unbound)) {
        return Err(UQAError::new(UQA_ERROR_STATE, "prepared statement has unbound parameters"));
    }
    let mut replacement = ResultData::prepare(&statement.session)?;
    replacement.native = NativeResult::Materialized(statement.session.execute(|native|
        native.execute_prepared_statement(statement.native(), &bindings.values))?);
    boundary::input_mut(result)?.data = Some(replacement);
    Ok(UQA_OK)
});
ffi_fn!(uqa_statement_finalize(statement: *mut *mut UQAStatement) {
    if statement.is_null() { return Ok(UQA_OK); }
    let pointer = *boundary::input(statement)?;
    if pointer.is_null() { return Ok(UQA_OK); }
    let session = Arc::clone(&boundary::input(pointer)?.session);
    let _call = session.enter_cleanup()?;
    boundary::release(statement)
});
