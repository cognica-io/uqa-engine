//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{self, Read};

struct Interrupted<'a> {
    bytes: &'a [u8],
    remaining: usize,
    token: &'a CancellationToken,
    mode: u8,
}
impl Read for Interrupted<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            match self.mode {
                0 => return Err(io::Error::other("injected rule image read failure")),
                1 => self.token.cancel(),
                _ => panic!("injected rule image unwind"),
            }
        }
        let width = output.len().min(self.remaining).max(1).min(output.len());
        let count = self.bytes.read(&mut output[..width])?;
        self.remaining = self.remaining.saturating_sub(count);
        Ok(count)
    }
}

#[test]
fn native_rule_images_retire_interrupted_input_and_terminal_output() {
    let source = fixture();
    let bytes = encode(&source);
    for mode in 0..3 {
        for remaining in [0, 128, bytes.len() / 2, bytes.len() - 1] {
            let memory = MemoryBudget::new(64 << 20);
            let token = CancellationToken::new();
            let mut reader = Interrupted {
                bytes: &bytes,
                remaining,
                token: &token,
                mode,
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                RuleCatalog::read_image(&mut reader, &memory, &token)
            }));
            if mode == 2 {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_err());
            }
            assert_eq!(memory.used(), 0);
        }
    }
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let captured = source
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    let denied = MemoryBudget::new(0);
    let mut output = captured.image_reader(&denied, &token).unwrap();
    assert!(output.read(&mut [0; 131]).is_err());
    assert!(output.read(&mut [0; 131]).is_err());
    assert_eq!(denied.used(), 0);
    let cancelled = CancellationToken::new();
    let scratch = MemoryBudget::new(16 << 10);
    let mut output = captured.image_reader(&scratch, &cancelled).unwrap();
    output.read(&mut [0; 131]).unwrap();
    cancelled.cancel();
    assert!(output.read(&mut [0; 131]).is_err());
    assert!(output.read(&mut [0; 131]).is_err());
    assert_eq!(scratch.used(), 0);
}

#[test]
fn native_rule_image_output_has_constant_preparation_and_bounded_payload_workspace() {
    let mut source = fixture();
    source
        .edit(&RelationIdentity::new("public", "items"), |entries| {
            entries.edit("watch", |value| {
                value.definition.actions = vec![Statement::Notify {
                    channel: "changes".into(),
                    payload: "x".repeat(128 << 10),
                }];
                Ok(())
            })?;
            Ok(())
        })
        .unwrap();
    let expected = encode(&source);
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let captured = source
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    drop(source);
    let scratch = MemoryBudget::new(16 << 10);
    let mut reader = None;
    assert_eq!(
        allocation_counter::measure(|| {
            reader = Some(captured.image_reader(&scratch, &token).unwrap());
        })
        .count_total,
        0
    );
    assert_eq!(drain(&mut reader.unwrap()), expected);
    assert_eq!(scratch.used(), 0);
}
