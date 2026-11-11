//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct Probe {
    reader: ImageRead,
    expected: usize,
    drops: Arc<AtomicUsize>,
    mode: &'static str,
    _padding: [u8; 257],
}

impl Future for Probe {
    type Output = ImageIoResult<()>;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        match self.mode {
            "ready" => Poll::Ready(Ok(())),
            "error" => Poll::Ready(Err(invalid())),
            "unwind" => panic!("native codec continuation"),
            _ => Poll::Pending,
        }
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        assert_eq!(self.reader.memory.used(), self.expected, "{}", self.mode);
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn native_continuation_keeps_original_header_until_future_destruction() {
    for mode in [
        "ready", "error", "unwind", "pending", "cancel", "window", "drop",
    ] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let mut input = Some(
            ImageDecoder::new(&memory, &token, |reader| Probe {
                expected: memory.used(),
                reader,
                drops: drops.clone(),
                mode,
                _padding: [0; 257],
            })
            .unwrap(),
        );
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        if mode == "drop" {
            drop(input.take());
        } else {
            if mode == "cancel" {
                token.cancel();
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                input.as_mut().unwrap().advance(
                    &mut &[][..],
                    ImageDecodeWindow {
                        bytes: usize::from(mode != "window"),
                        steps: 1,
                    },
                )
            }));
            match mode {
                "ready" => assert!(matches!(result, Ok(Ok(Some(()))))),
                "unwind" => assert!(result.is_err()),
                _ => assert!(matches!(result, Ok(Err(_)))),
            }
            assert!(input
                .as_mut()
                .unwrap()
                .advance(&mut &[][..], ImageDecodeWindow { bytes: 1, steps: 1 })
                .is_err());
        }
        assert_eq!(drops.load(Ordering::SeqCst), 1, "{mode}");
        assert_eq!(memory.used(), 0, "{mode}");
    }
}

#[test]
fn native_continuation_admits_before_constructor_and_retires_constructor_unwind() {
    let memory = MemoryBudget::new(size_of::<Probe>());
    let token = CancellationToken::new();
    let result = ImageDecoder::<()>::new(&memory, &token, |_| -> Probe {
        panic!("constructor ran before channel admission")
    });
    assert!(matches!(result, Err(ImageIoError::Memory(_))));
    assert_eq!(memory.used(), 0);

    let memory = MemoryBudget::new(1 << 20);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ImageDecoder::<()>::new(&memory, &token, |_| -> Probe {
            panic!("native input constructor")
        })
    }));
    assert!(result.is_err());
    assert_eq!(memory.used(), 0);
}
