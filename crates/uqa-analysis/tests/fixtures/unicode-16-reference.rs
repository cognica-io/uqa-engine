//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent reference stream: compile with rustc +1.90.0 and SHA-256 stdout.

use std::io::{self, Write};

fn main() -> io::Result<()> {
    assert_eq!(char::UNICODE_VERSION, (16, 0, 0));
    let mut output = io::BufWriter::new(io::stdout().lock());
    for scalar in 0..=0x0010_ffff {
        let Some(character) = char::from_u32(scalar) else {
            continue;
        };
        output.write_all(&scalar.to_le_bytes())?;
        output.write_all(&[u8::from(character.is_whitespace())])?;
        for (prefix, suffix) in [("A", "Σ"), ("AΣ", ""), ("AΣ", "A")] {
            let lowered = format!("{prefix}{character}{suffix}").to_lowercase();
            output.write_all(&(lowered.len() as u64).to_le_bytes())?;
            output.write_all(lowered.as_bytes())?;
        }
    }
    output.flush()
}
