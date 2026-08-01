//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// libfuzzer target: `uqa_graph::cypher::parse_cypher` must not panic.
// Run with: cargo +nightly fuzz run cypher_parse

#![no_main]

use libfuzzer_sys::fuzz_target;
use uqa_graph::cypher::parse_cypher;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = parse_cypher(s);
    }
});
