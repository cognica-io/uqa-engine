// Unified Query Algebra
// Copyright (c) 2023-2026 Cognica, Inc.
//
// libfuzzer target: `uqa_sql::compile` must not panic on any input.
// Run with: cargo +nightly fuzz run sql_compile

#![no_main]

use libfuzzer_sys::fuzz_target;
use uqa_sql::compile;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = compile(s);
    }
});
