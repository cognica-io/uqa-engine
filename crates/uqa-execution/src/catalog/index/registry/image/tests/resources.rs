//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn native_index_images_release_partial_input_on_cancellation_io_and_unwind() {
    struct Interrupted<'a> {
        bytes: &'a [u8],
        token: &'a CancellationToken,
        calls: usize,
        mode: u8,
    }
    impl Read for Interrupted<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls == 19 {
                match self.mode {
                    0 => self.token.cancel(),
                    1 => return Err(std::io::Error::other("injected index image input failure")),
                    _ => panic!("injected index image input unwind"),
                }
            }
            self.bytes.read(output)
        }
    }
    let bytes = wire(&[row("a", Some("one")), row("b", Some("two"))]);
    let memory = MemoryBudget::new(1 << 20);
    for mode in 0..3 {
        let cancellation = CancellationToken::new();
        let mut input = Interrupted {
            bytes: &bytes,
            token: &cancellation,
            calls: 0,
            mode,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            CatalogIndexes::read_image(&mut input, &memory, &cancellation)
        }));
        if mode == 2 {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_err());
        }
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_index_image_output_rejects_invalid_native_identifiers() {
    for name in ["", "bad\0name"] {
        let mut source = CatalogIndexes::default();
        source
            .insert(row(name, None), RelationPersistence::Permanent)
            .unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let captured = source.capture(&control).unwrap();
        let output = StorageReadControl::with_limit(4096);
        let mut reader = captured.image_reader(&output).unwrap();
        assert!(reader.read(&mut [0; 4096]).is_err());
        assert_eq!(output.memory().used(), 0);
        assert!(reader.read(&mut [0]).is_err());
    }
}
