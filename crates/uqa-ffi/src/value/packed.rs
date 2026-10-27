//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed UUID octets and packed bit strings retain the native scalar owner.

use super::{boundary, wrong_type, ScalarAccess, UQAError, UQAValue};
use crate::abi::{UQABitsView, UQABytesView, UQAStringView, UQA_OK};
use uqa_core::{memory::ProductionControl, Value};
use uqa_sql::{expr::bit_string::BitStringKind, SQLParam};

ffi_fn!(uqa_value_create_uuid_bytes(data: *const u8, size: usize, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    if size != 16 { return Err(UQAError::invalid("UUID input must contain exactly sixteen bytes")); }
    let bytes = boundary::span(data, size)?.try_into().map_err(|_| UQAError::invalid("UUID input must contain exactly sixteen bytes"))?;
    let parameter = SQLParam::uuid_with_control(bytes, &ProductionControl::uncontrolled())?;
    *out = Box::into_raw(UQAValue::owned(parameter, None)?);
    Ok(UQA_OK)
});

macro_rules! bit_constructor {
    ($name:ident, $kind:ident) => {
        ffi_fn!($name(data: *const u8, size: usize, bit_length: u32, out: *mut *mut UQAValue) {
            let out = boundary::new_handle(out)?;
            if size != bit_length.div_ceil(8) as usize {
                return Err(UQAError::invalid("packed input size must match its logical bit length"));
            }
            let bytes = boundary::span(data, size)?;
            let parameter = SQLParam::packed_bits_with_control(bytes, bit_length, BitStringKind::$kind, &ProductionControl::uncontrolled())?;
            *out = Box::into_raw(UQAValue::owned(parameter, None)?);
            Ok(UQA_OK)
        });
    };
}

bit_constructor!(uqa_value_create_bit, Fixed);
bit_constructor!(uqa_value_create_varbit, Varying);

ffi_fn!(uqa_value_get_uuid_bytes(value: *const UQAValue, out: *mut UQABytesView) {
    let out = boundary::output(out)?;
    let value = boundary::input(value)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    value.check_access(ScalarAccess::Uuid)?;
    let observed = value.observed()?;
    let Value::Str(text) = observed else { return Err(wrong_type(observed)); };
    if value.uuid_bytes.get().is_none() {
        let bytes = uqa_sql::expr::parse_uuid_bytes_with_control(text, &value.control())?;
        let _ = value.uuid_bytes.set(bytes);
    }
    let bytes = value.uuid_bytes.get().ok_or_else(|| UQAError::state("UUID decoder returned no octets"))?;
    *out = UQABytesView { data: bytes.as_ptr(), size: bytes.len() };
    Ok(UQA_OK)
});

ffi_fn!(uqa_value_get_uuid(value: *const UQAValue, out: *mut UQAStringView) {
    let out = boundary::output(out)?;
    let value = boundary::input(value)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    value.check_access(ScalarAccess::Uuid)?;
    match value.observed()? {
        Value::Str(text) => { *out = text.as_str().into(); Ok(UQA_OK) },
        other => Err(wrong_type(other)),
    }
});

macro_rules! bit_getter {
    ($name:ident, $access:ident) => {
        ffi_fn!($name(value: *const UQAValue, out: *mut UQABitsView) {
            let out = boundary::output(out)?;
            let value = boundary::input(value)?;
            let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
            value.check_access(ScalarAccess::$access)?;
            match value.observed()? {
                Value::BitString(bits) => {
                    *out = UQABitsView { data: bits.as_bytes().as_ptr(), size: bits.as_bytes().len(), bit_length: bits.bit_len() };
                    Ok(UQA_OK)
                },
                other => Err(wrong_type(other)),
            }
        });
    };
}

bit_getter!(uqa_value_get_bit, Bit);
bit_getter!(uqa_value_get_varbit, VarBit);
