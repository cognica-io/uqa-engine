//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn partial_and_interrupted_io_preserves_bytes_and_bounded_attempts() {
    struct Partial {
        bytes: Vec<u8>,
        calls: usize,
    }
    impl Write for Partial {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            assert!(bytes.len() <= CHUNK);
            self.calls += 1;
            if self.calls % 3 == 1 {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            let length = bytes.len().min(7);
            self.bytes.extend_from_slice(&bytes[..length]);
            Ok(length)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    struct Source<'a> {
        bytes: &'a [u8],
        calls: usize,
    }
    impl Read for Source<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            assert!(output.len() <= CHUNK);
            self.calls += 1;
            if self.calls % 3 == 1 {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            let length = output.len().min(5).min(self.bytes.len());
            output[..length].copy_from_slice(&self.bytes[..length]);
            self.bytes = &self.bytes[length..];
            Ok(length)
        }
    }
    let cancellation = CancellationToken::new();
    let expected = vec![63; 10003];
    let mut destination = Partial {
        bytes: vec![],
        calls: 0,
    };
    let mut output = ImageOutput {
        writer: &mut destination,
        cancellation: &cancellation,
        written: 0,
    };
    output.sized(&expected).unwrap();
    assert_eq!(output.written, 10011);
    let mut source = Source {
        bytes: &destination.bytes,
        calls: 0,
    };
    let memory = MemoryBudget::new(10003);
    let mut input = ImageInput {
        reader: &mut source,
        memory: &memory,
        cancellation: &cancellation,
    };
    let decoded = input.bytes().unwrap();
    input.end().unwrap();
    assert_eq!(&**decoded, &expected);
    assert_eq!(memory.used(), 10003);
    drop(decoded);
    assert_eq!(memory.used(), 0);
}

#[test]
fn framing_rejects_truncation_trailing_bytes_and_unadmitted_lengths() {
    let cancellation = CancellationToken::new();
    for source in [
        &[1_u8][..],
        &[1, 0, 0, 0, 0, 0, 0, 0][..],
        &[1, 0, 0, 0, 0, 0, 0, 0, 255][..],
    ] {
        let memory = MemoryBudget::new(1024);
        let mut source = source;
        let mut input = ImageInput {
            reader: &mut source,
            memory: &memory,
            cancellation: &cancellation,
        };
        assert!(input.text().is_err());
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(3);
    let mut source = &[4, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4][..];
    let mut input = ImageInput {
        reader: &mut source,
        memory: &memory,
        cancellation: &cancellation,
    };
    assert!(matches!(
        input.bytes(),
        Err(ImageIoError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(source, [1, 2, 3, 4]);
    assert_eq!(memory.used(), 0);
    let mut source = &[4, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4][..];
    let mut input = ImageInput {
        reader: &mut source,
        memory: &memory,
        cancellation: &cancellation,
    };
    assert!(matches!(
        input.bytes_with_limit(2),
        Err(ImageIoError::Memory(MemoryError::Limit {
            required: 4,
            limit: 2
        }))
    ));
    assert!(input.end().is_err());
}

#[test]
fn interrupted_io_observes_cancellation_and_keeps_partial_write_count() {
    struct Interrupt<'a> {
        cancellation: &'a CancellationToken,
    }
    impl Write for Interrupt<'_> {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            self.cancellation.cancel();
            Err(std::io::ErrorKind::Interrupted.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Read for Interrupt<'_> {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            self.cancellation.cancel();
            Err(std::io::ErrorKind::Interrupted.into())
        }
    }
    struct Fails {
        once: bool,
    }
    impl Write for Fails {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            if self.once {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            self.once = true;
            Ok(2)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let cancellation = CancellationToken::new();
    let mut writer = Interrupt {
        cancellation: &cancellation,
    };
    let mut output = ImageOutput {
        writer: &mut writer,
        cancellation: &cancellation,
        written: 11,
    };
    assert!(matches!(
        output.bytes(&[1]),
        Err(ImageIoError::Cancelled(_))
    ));
    assert_eq!(output.written, 11);
    for trailing in [false, true] {
        let cancellation = CancellationToken::new();
        let memory = MemoryBudget::new(8);
        let mut reader = Interrupt {
            cancellation: &cancellation,
        };
        let mut input = ImageInput {
            reader: &mut reader,
            memory: &memory,
            cancellation: &cancellation,
        };
        let result = if trailing {
            input.end()
        } else {
            input.exact(&mut [0; 1])
        };
        assert!(matches!(result, Err(ImageIoError::Cancelled(_))));
    }
    let cancellation = CancellationToken::new();
    let mut writer = Fails { once: false };
    let mut output = ImageOutput {
        writer: &mut writer,
        cancellation: &cancellation,
        written: 7,
    };
    assert!(matches!(output.bytes(&[1, 2, 3]), Err(ImageIoError::Io(_))));
    assert_eq!(output.written, 9);
}

#[test]
fn invalid_writer_progress_and_count_overflow_are_errors() {
    struct Count(usize);
    impl Write for Count {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Ok(self.0)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let cancellation = CancellationToken::new();
    for count in [0, 2] {
        let mut writer = Count(count);
        let mut output = ImageOutput {
            writer: &mut writer,
            cancellation: &cancellation,
            written: 0,
        };
        assert!(output.bytes(&[1]).is_err());
    }
    let mut writer = Count(1);
    let mut output = ImageOutput {
        writer: &mut writer,
        cancellation: &cancellation,
        written: u64::MAX,
    };
    assert!(matches!(
        output.bytes(&[1]),
        Err(ImageIoError::Memory(MemoryError::SizeOverflow))
    ));
}
