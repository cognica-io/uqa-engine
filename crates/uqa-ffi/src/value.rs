//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable C values borrow native carriers or own admitted parameter copies.

use std::cell::OnceCell;
use std::ffi::c_char;
use std::sync::Arc;
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionControl, ProductionVec},
    Value,
};
use uqa_sql::{ast::ColumnType, SQLParam};

use crate::{
    abi::{
        UQABytesView, UQAStringView, UQA_DONE, UQA_ERROR_NULL, UQA_ERROR_RANGE, UQA_ERROR_TYPE,
        UQA_OK,
    },
    boundary,
    error::UQAError,
    session::Session,
    types::UQAType,
};

mod scalars;
pub use scalars::*;

enum ValueSource {
    Owned(Produced<SQLParam>),
    // The containing result owns these addresses until refill or release. No destructor owns them.
    Borrowed {
        value: *const Value,
        ty: Option<*const ColumnType>,
    },
}

pub struct UQAValue {
    source: ValueSource,
    decoded: OnceCell<Produced<Value>>,
    numeric_text: OnceCell<Produced<String>>,
    parameter_type: Option<ColumnType>,
    pub(crate) session: Option<Arc<Session>>,
    _memory: Option<MemoryReservation>,
}

impl UQAValue {
    pub(crate) fn borrowed(
        value: &Value,
        ty: Option<&ColumnType>,
        session: &Arc<Session>,
    ) -> boundary::Result<Box<Self>> {
        let memory = session.memory.reserve(size_of::<Self>())?;
        boundary::boxed(Self {
            source: ValueSource::Borrowed {
                value,
                ty: ty.map(std::ptr::from_ref),
            },
            decoded: OnceCell::new(),
            numeric_text: OnceCell::new(),
            parameter_type: None,
            session: Some(Arc::clone(session)),
            _memory: Some(memory),
        })
    }

    fn owned(
        value: Produced<SQLParam>,
        session: Option<Arc<Session>>,
    ) -> boundary::Result<Box<Self>> {
        let parameter_type = match &*value {
            SQLParam::Vector(values) => {
                Some(ColumnType::Vector(u32::try_from(values.len()).map_err(
                    |_| UQAError::new(UQA_ERROR_RANGE, "vector dimensions exceed the SQL range"),
                )?))
            }
            _ => None,
        };
        let memory = session
            .as_ref()
            .map(|session| session.memory.reserve(size_of::<Self>()))
            .transpose()?;
        boundary::boxed(Self {
            source: ValueSource::Owned(value),
            decoded: OnceCell::new(),
            numeric_text: OnceCell::new(),
            parameter_type,
            session,
            _memory: memory,
        })
    }

    pub(crate) fn control(&self) -> ProductionControl<'_> {
        self.session
            .as_ref()
            .map_or_else(ProductionControl::uncontrolled, |session| session.control())
    }

    pub(crate) fn raw(&self) -> Option<&Value> {
        match &self.source {
            ValueSource::Owned(parameter) => parameter.scalar_value(),
            ValueSource::Borrowed { value, .. } => {
                // Safety: only the containing result creates this pointer; C keeps that owner live.
                #[allow(unsafe_code)]
                unsafe {
                    Some(&**value)
                }
            }
        }
    }

    pub(crate) fn ty(&self) -> Option<&ColumnType> {
        match &self.source {
            ValueSource::Owned(parameter) => parameter
                .declared_scalar_type()
                .or(self.parameter_type.as_ref()),
            ValueSource::Borrowed { ty, .. } => ty.map(|ty| {
                // Safety: the immutable descriptor belongs to the same live result as the value.
                #[allow(unsafe_code)]
                unsafe {
                    &*ty
                }
            }),
        }
    }

    fn observed(&self) -> boundary::Result<&Value> {
        let Some(value) = self.raw() else {
            return Err(UQAError::new(
                UQA_ERROR_TYPE,
                "value is a vector or tensor parameter",
            ));
        };
        let Value::Datum(datum) = value else {
            return Ok(value);
        };
        if let Some(decoded) = self.decoded.get() {
            return Ok(decoded);
        }
        let decoded = uqa_sql::expr::datums::read_with_catalog_and_control(
            datum,
            self.session
                .as_ref()
                .map(|session| &session.native as &dyn uqa_sql::expr::EngineHook),
            &self.control(),
        )?;
        // Calls observing one value are serialized by the C lifetime contract and session admission.
        self.decoded
            .set(decoded)
            .map_err(|_| UQAError::state("value was already decoded"))?;
        self.decoded
            .get()
            .map(|value| &**value)
            .ok_or_else(|| UQAError::state("value decoder returned no value"))
    }

    fn check_access(&self, access: ScalarAccess) -> boundary::Result<()> {
        if matches!(self.raw(), Some(Value::Null)) {
            return Err(wrong_type(&Value::Null));
        }
        let Some(mut ty) = self.ty() else {
            return Ok(());
        };
        while let ColumnType::Domain { base, .. } = ty {
            ty = base;
        }
        let accepted = match access {
            ScalarAccess::Bool => matches!(ty, ColumnType::Boolean),
            ScalarAccess::Int16 => matches!(ty, ColumnType::SmallInteger),
            ScalarAccess::Int32 => matches!(ty, ColumnType::SmallInteger | ColumnType::Integer),
            ScalarAccess::Integer => matches!(
                ty,
                ColumnType::SmallInteger
                    | ColumnType::Integer
                    | ColumnType::BigInteger
                    | ColumnType::Oid
                    | ColumnType::Xid
                    | ColumnType::Regproc
                    | ColumnType::Regprocedure
                    | ColumnType::Regclass
                    | ColumnType::Regnamespace
                    | ColumnType::Regcollation
                    | ColumnType::Regrole
                    | ColumnType::Regtype
            ),
            ScalarAccess::Float => matches!(ty, ColumnType::Real | ColumnType::DoublePrecision),
            ScalarAccess::Float32 => matches!(ty, ColumnType::Real),
            ScalarAccess::Numeric => matches!(ty, ColumnType::Numeric { .. }),
            ScalarAccess::Json => matches!(ty, ColumnType::Json),
            ScalarAccess::JsonB => matches!(ty, ColumnType::JsonB),
            ScalarAccess::Text => matches!(
                ty,
                ColumnType::Text
                    | ColumnType::Varchar(_)
                    | ColumnType::Bpchar
                    | ColumnType::Character(_)
                    | ColumnType::Name
                    | ColumnType::RefCursor
                    | ColumnType::Xml
            ),
            ScalarAccess::Bytes => matches!(ty, ColumnType::Bytea),
        };
        if accepted {
            Ok(())
        } else {
            Err(UQAError::new(
                UQA_ERROR_TYPE,
                "SQL type does not support the requested accessor",
            ))
        }
    }

    pub(crate) fn copy_parameter(
        &self,
        control: &ProductionControl<'_>,
    ) -> boundary::Result<Produced<SQLParam>> {
        if let ValueSource::Owned(parameter) = &self.source {
            match &**parameter {
                SQLParam::Vector(values) => {
                    let mut copy = ProductionVec::new(*control);
                    copy.extend_from_slice(values)?;
                    let (values, memory) = copy.finish()?.into_parts();
                    return Ok(control.finish(SQLParam::Vector(values), memory)?);
                }
                SQLParam::Tensor(vectors) => {
                    let mut copy = ProductionVec::new(*control);
                    copy.reserve(vectors.len())?;
                    for values in vectors {
                        let mut vector = ProductionVec::new(*control);
                        vector.extend_from_slice(values)?;
                        copy.push_produced(vector.finish()?)?;
                    }
                    let (vectors, memory) = copy.finish()?.into_parts();
                    return Ok(control.finish(SQLParam::Tensor(vectors), memory)?);
                }
                SQLParam::DeferredError { .. } => {
                    return Err(UQAError::state(
                        "internal deferred parameter is not a C value",
                    ))
                }
                SQLParam::Scalar(_) | SQLParam::TypedScalar { .. } => {}
            }
        }
        let value = self
            .raw()
            .ok_or_else(|| UQAError::state("missing native value"))?;
        let value = control.copy_value(value)?;
        if let Some(ty) = self.ty() {
            let ty = ty.clone_with_control(control)?;
            let (value, value_memory) = value.into_parts();
            let (ty, type_memory) = ty.into_parts();
            Ok(control.finish(
                SQLParam::TypedScalar { value, ty },
                control.combine(value_memory, type_memory),
            )?)
        } else {
            let (value, memory) = value.into_parts();
            Ok(control.finish(SQLParam::Scalar(value), memory)?)
        }
    }
}

fn scalar(value: Value, ty: Option<ColumnType>) -> boundary::Result<*mut UQAValue> {
    let parameter = match ty {
        Some(ty) => SQLParam::typed_scalar(value, ty),
        None => SQLParam::Scalar(value),
    };
    let parameter = ProductionControl::uncontrolled().finish(parameter, None)?;
    Ok(Box::into_raw(UQAValue::owned(parameter, None)?))
}

ffi_fn!(uqa_value_create_null(out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    *out = scalar(Value::Null, None)?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_value_create_int64(value: i64, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    *out = scalar(Value::Int(value), Some(ColumnType::BigInteger))?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_value_create_int16(value: i16, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    *out = scalar(Value::Int(i64::from(value)), Some(ColumnType::SmallInteger))?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_value_create_int32(value: i32, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    *out = scalar(Value::Int(i64::from(value)), Some(ColumnType::Integer))?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_value_create_bool(value: u8, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    if value > 1 { return Err(UQAError::invalid("boolean input must be 0 or 1")); }
    *out = scalar(Value::Bool(value != 0), Some(ColumnType::Boolean))?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_value_create_float64(value: f64, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    *out = scalar(Value::Float(value), Some(ColumnType::DoublePrecision))?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_value_create_float32(value: f32, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    *out = scalar(Value::Float(f64::from(value)), Some(ColumnType::Real))?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_value_create_text(value: *const c_char, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    *out = scalar(Value::Str(boundary::copy_text(boundary::text(value)?)?), Some(ColumnType::Text))?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_value_create_text_n(value: *const c_char, size: usize, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    *out = scalar(Value::Str(boundary::copy_text(boundary::text_n(value, size)?)?), Some(ColumnType::Text))?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_value_create_bytes(data: *const u8, size: usize, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    let input = boundary::span(data, size)?;
    let mut bytes = ProductionVec::new(ProductionControl::uncontrolled());
    bytes.extend_from_slice(input)?;
    let (bytes, _) = bytes.finish()?.into_parts();
    *out = scalar(Value::Bytes(bytes), Some(ColumnType::Bytea))?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_value_create_vector_f32(data: *const f32, dimensions: usize, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    let input = boundary::span(data, dimensions)?;
    let control = ProductionControl::uncontrolled();
    *out = Box::into_raw(UQAValue::owned(SQLParam::vector_with_control(input, &control)?, None)?);
    Ok(UQA_OK)
});
ffi_fn!(uqa_value_create_copy(value: *const UQAValue, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    let value = boundary::input(value)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    *out = Box::into_raw(UQAValue::owned(value.copy_parameter(&value.control())?, value.session.clone())?);
    Ok(UQA_OK)
});
ffi_fn!(uqa_value_destroy(value: *mut *mut UQAValue) { boundary::release(value) });
ffi_fn!(uqa_value_get_is_null(value: *const UQAValue, out: *mut u8) {
    let out = boundary::output(out)?;
    let value = boundary::input(value)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    *out = u8::from(matches!(value.raw(), Some(Value::Null)));
    Ok(UQA_OK)
});

ffi_fn!(uqa_value_get_type(value: *const UQAValue, out: *mut *const UQAType) {
    let out = boundary::output(out)?;
    let value = boundary::input(value)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    match value.ty() {
        Some(ty) => { *out = crate::types::view(ty); Ok(UQA_OK) },
        None => Ok(UQA_DONE),
    }
});

fn wrong_type(value: &Value) -> UQAError {
    if matches!(value, Value::Null) {
        UQAError::new(UQA_ERROR_NULL, "value is SQL NULL")
    } else {
        UQAError::new(
            UQA_ERROR_TYPE,
            "value does not have the requested representation",
        )
    }
}

#[derive(Clone, Copy)]
enum ScalarAccess {
    Bool,
    Int16,
    Int32,
    Integer,
    Float32,
    Float,
    Numeric,
    Json,
    JsonB,
    Text,
    Bytes,
}

macro_rules! scalar_getter {
    ($name:ident, $output:ty, $access:ident, $pattern:pat => $value:expr) => {
        ffi_fn!($name(value: *const UQAValue, out: *mut $output) {
            let out = boundary::output(out)?;
            let value = boundary::input(value)?;
            let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
            value.check_access(ScalarAccess::$access)?;
            match value.observed()? {
                $pattern => { *out = $value; Ok(UQA_OK) },
                value => Err(wrong_type(value)),
            }
        });
    };
}
scalar_getter!(uqa_value_get_int64, i64, Integer, Value::Int(value) => *value);
scalar_getter!(uqa_value_get_bool, u8, Bool, Value::Bool(value) => u8::from(*value));
scalar_getter!(uqa_value_get_float64, f64, Float, Value::Float(value) => *value);
scalar_getter!(uqa_value_get_text, UQAStringView, Text, Value::Str(value) | Value::FixedChar(value) => value.as_str().into());
scalar_getter!(uqa_value_get_bytes, UQABytesView, Bytes, Value::Bytes(value) => UQABytesView { data: value.as_ptr(), size: value.len() });
scalar_getter!(uqa_value_get_json, UQAStringView, Json, Value::Json(value) => value.as_str().into());
scalar_getter!(uqa_value_get_jsonb, UQAStringView, JsonB, Value::JsonB(value) => value.as_str().into());

pub struct UQABuffer {
    text: Option<Produced<String>>,
}
ffi_fn!(uqa_buffer_create(out: *mut *mut UQABuffer) {
    let out = boundary::new_handle(out)?;
    *out = Box::into_raw(boundary::boxed(UQABuffer { text: None })?);
    Ok(UQA_OK)
});
ffi_fn!(uqa_buffer_destroy(buffer: *mut *mut UQABuffer) { boundary::release(buffer) });
ffi_fn!(uqa_buffer_get_bytes(buffer: *mut UQABuffer, out: *mut UQABytesView) {
    let out = boundary::output(out)?;
    if let Some(text) = &boundary::input(buffer)?.text {
        *out = UQABytesView { data: text.as_ptr(), size: text.len() };
    }
    Ok(UQA_OK)
});

ffi_fn!(uqa_value_format_sql(value: *const UQAValue, buffer: *mut UQABuffer) {
    let value = boundary::input(value)?;
    boundary::input(buffer)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    let control = value.control();
    let materialized;
    let native = if let Some(native) = value.raw() { native } else {
        let ValueSource::Owned(parameter) = &value.source else { return Err(UQAError::state("missing native value")); };
        materialized = parameter.to_value_with_control(&control)?;
        &materialized
    };
    let fallback;
    let ty = if let Some(ty) = value.ty() { ty } else {
        fallback = match (&value.source, native) {
            (_, Value::Null | Value::Str(_)) => ColumnType::Text,
            (_, Value::Bool(_)) => ColumnType::Boolean,
            (_, Value::Int(_)) => ColumnType::BigInteger,
            (_, Value::Float(_)) => ColumnType::DoublePrecision,
            (_, Value::Bytes(_)) => ColumnType::Bytea,
            (ValueSource::Owned(parameter), _) => match &**parameter {
                SQLParam::Vector(values) => ColumnType::Vector(u32::try_from(values.len()).map_err(|_| UQAError::new(UQA_ERROR_RANGE, "vector dimensions exceed the SQL range"))?),
                _ => return Err(UQAError::state("value has no SQL output descriptor")),
            },
            _ => return Err(UQAError::state("value has no SQL output descriptor")),
        };
        &fallback
    };
    let text = uqa_sql::result::format_postgres_text_with_control(
        native, ty, value.session.as_ref().map(|session| &session.native as &dyn uqa_sql::expr::EngineHook), &control,
    )?;
    boundary::input_mut(buffer)?.text = Some(text);
    Ok(UQA_OK)
});
