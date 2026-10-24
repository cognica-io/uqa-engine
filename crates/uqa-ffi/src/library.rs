//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static library metadata; string views live for the loaded library's lifetime.

use crate::{
    abi::{
        UQAStringView, UQA_CAP_ARROW_FDW, UQA_CAP_DUCKDB_FDW, UQA_CAP_KUROMOJI, UQA_CAP_NORI,
        UQA_OK,
    },
    boundary,
};

pub(crate) const fn capabilities() -> u64 {
    (if cfg!(feature = "nori") {
        UQA_CAP_NORI
    } else {
        0
    }) | (if cfg!(feature = "kuromoji") {
        UQA_CAP_KUROMOJI
    } else {
        0
    }) | (if cfg!(feature = "arrow") {
        UQA_CAP_ARROW_FDW
    } else {
        0
    }) | (if cfg!(feature = "duckdb") {
        UQA_CAP_DUCKDB_FDW
    } else {
        0
    })
}

ffi_fn!(uqa_library_get_abi_version(out: *mut u32) {
    let out = boundary::output(out)?;
    *out = 1;
    Ok(UQA_OK)
});
ffi_fn!(uqa_library_get_version(out: *mut UQAStringView) {
    let out = boundary::output(out)?;
    *out = env!("CARGO_PKG_VERSION").into();
    Ok(UQA_OK)
});
ffi_fn!(uqa_library_get_source_revision(out: *mut UQAStringView) {
    let out = boundary::output(out)?;
    *out = option_env!("UQA_SOURCE_REVISION").unwrap_or("unknown").into();
    Ok(UQA_OK)
});
ffi_fn!(uqa_library_get_capabilities(out: *mut u64) {
    let out = boundary::output(out)?;
    *out = capabilities();
    Ok(UQA_OK)
});
