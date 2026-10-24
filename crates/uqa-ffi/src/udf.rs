//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owned runtime function options using the native execution contract.

use crate::{
    abi::{UQA_OK, UQA_VOLATILITY_IMMUTABLE, UQA_VOLATILITY_STABLE, UQA_VOLATILITY_VOLATILE},
    boundary,
    error::UQAError,
};
use uqa_engine::{SQLFunctionOptions, SQLFunctionVolatility};

#[derive(Default)]
pub struct UQAUDFOptions {
    options: SQLFunctionOptions,
}

ffi_fn!(uqa_udf_options_create(out: *mut *mut UQAUDFOptions) {
    let out = boundary::new_handle(out)?;
    *out = Box::into_raw(boundary::boxed(UQAUDFOptions::default())?);
    Ok(UQA_OK)
});

ffi_fn!(uqa_udf_options_destroy(options: *mut *mut UQAUDFOptions) {
    boundary::release(options)
});

ffi_fn!(uqa_udf_options_get_volatility(options: *mut UQAUDFOptions, out: *mut u32) {
    let out = boundary::output(out)?;
    *out = match boundary::input(options)?.options.volatility {
        SQLFunctionVolatility::Immutable => UQA_VOLATILITY_IMMUTABLE,
        SQLFunctionVolatility::Stable => UQA_VOLATILITY_STABLE,
        SQLFunctionVolatility::Volatile => UQA_VOLATILITY_VOLATILE,
    };
    Ok(UQA_OK)
});

ffi_fn!(uqa_udf_options_set_volatility(options: *mut UQAUDFOptions, volatility: u32) {
    let volatility = match volatility {
        UQA_VOLATILITY_IMMUTABLE => SQLFunctionVolatility::Immutable,
        UQA_VOLATILITY_STABLE => SQLFunctionVolatility::Stable,
        UQA_VOLATILITY_VOLATILE => SQLFunctionVolatility::Volatile,
        _ => return Err(UQAError::invalid("unknown UDF volatility")),
    };
    boundary::input_mut(options)?.options.volatility = volatility;
    Ok(UQA_OK)
});

ffi_fn!(uqa_udf_options_get_may_mutate_engine(options: *mut UQAUDFOptions, out: *mut u8) {
    let out = boundary::output(out)?;
    *out = u8::from(boundary::input(options)?.options.may_mutate_engine);
    Ok(UQA_OK)
});

ffi_fn!(uqa_udf_options_set_may_mutate_engine(options: *mut UQAUDFOptions, may_mutate_engine: u8) {
    let value = match may_mutate_engine {
        0 => false,
        1 => true,
        _ => return Err(UQAError::invalid("may_mutate_engine must be zero or one")),
    };
    boundary::input_mut(options)?.options.may_mutate_engine = value;
    Ok(UQA_OK)
});
