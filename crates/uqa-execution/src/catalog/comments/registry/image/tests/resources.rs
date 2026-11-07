//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct Interrupted<'a> {
    bytes: &'a [u8],
    token: &'a CancellationToken,
    calls: usize,
    unwind: bool,
}

impl Read for Interrupted<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        self.calls += 1;
        if self.calls == 9 {
            assert!(!self.unwind, "injected comment input unwind");
            self.token.cancel();
        }
        self.bytes.read(output)
    }
}

struct Failed;

impl Read for Failed {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("injected input error"))
    }
}

#[test]
fn native_comment_images_release_partial_input_on_cancellation_io_and_unwind() {
    let complete = wire(&[
        (address(1, 0, 1259), b"first"),
        (address(2, 0, 1259), b"second"),
    ]);
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    for unwind in [false, true] {
        let mut input = Interrupted {
            bytes: &complete,
            token: &token,
            calls: 0,
            unwind,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            CommentRegistry::read_image(&mut input, &memory, &token)
        }));
        if unwind {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("57014"));
        }
        assert_eq!(memory.used(), 0);
        token.reset();
    }
    assert!(CommentRegistry::read_image(&mut Failed, &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
    drop(CommentRegistry::read_image(&mut &complete[..], &memory, &token).unwrap());
    assert_eq!(memory.used(), 0);
}
