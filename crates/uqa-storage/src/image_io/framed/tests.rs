//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct Bytes<'a>(&'a [u8]);
impl ImageRead for Bytes<'_> {
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        let count = output.len().min(self.0.len()).min(37);
        output[..count].copy_from_slice(&self.0[..count]);
        self.0 = &self.0[count..];
        Ok(count)
    }
}

fn encode(bytes: &[u8], width: usize) -> Vec<u8> {
    let control = StorageReadControl::with_limit(16 << 10);
    let source = ImageReader::new(Bytes(bytes), &control).unwrap();
    let mut reader = framed_image_reader(source, &control).unwrap();
    let mut result = Vec::new();
    let mut buffer = vec![0; width];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        result.extend_from_slice(&buffer[..count]);
    }
    assert_eq!(control.memory().used(), 0);
    result
}

#[test]
fn framed_owner_images_have_canonical_independent_bytes_and_exact_child_eof() {
    for length in [0, 1, 4095, 4096, 4097, 16384] {
        let value = vec![31; length];
        let mut expected = Vec::new();
        for chunk in value.chunks(CHUNK) {
            expected.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            expected.extend_from_slice(chunk);
        }
        expected.extend_from_slice(&0u32.to_le_bytes());
        for width in [1, 19, 4096, 20000] {
            assert_eq!(encode(&value, width), expected);
        }
        expected.extend_from_slice(b"parent tail");
        let mut input = expected.as_slice();
        {
            let mut child = FramedImageInput::new(&mut input);
            let mut restored = Vec::new();
            child.read_to_end(&mut restored).unwrap();
            assert_eq!(restored, value);
            assert_eq!(child.read(&mut [0; 1]).unwrap(), 0);
        }
        assert_eq!(input, b"parent tail");
    }
}

#[test]
fn framed_owner_inputs_reject_truncation_noncanonical_chunks_and_resume_after_interrupt() {
    struct Partial<'a> {
        bytes: &'a [u8],
        attempt: usize,
    }
    impl Read for Partial<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.attempt += 1;
            if self.attempt.is_multiple_of(3) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            self.bytes.read(&mut output[..1])
        }
    }
    let bytes = encode(b"payload", 3);
    let mut partial = Partial {
        bytes: &bytes,
        attempt: 0,
    };
    let mut restored = Vec::new();
    FramedImageInput::new(&mut partial)
        .read_to_end(&mut restored)
        .unwrap();
    assert_eq!(restored, b"payload");
    for end in 0..bytes.len() {
        let mut source = &bytes[..end];
        let mut input = FramedImageInput::new(&mut source);
        assert!(input.read_to_end(&mut Vec::new()).is_err());
        assert!(input.read(&mut [0; 1]).is_err());
    }
    for invalid in [
        vec![1, 0, 0, 0, 9, 1, 0, 0, 0, 8, 0, 0, 0, 0],
        vec![1, 16, 0, 0],
    ] {
        let mut source = invalid.as_slice();
        assert!(FramedImageInput::new(&mut source)
            .read_to_end(&mut Vec::new())
            .is_err());
    }
}

#[test]
fn framed_owner_cursors_enforce_admission_cancellation_and_unwind_retirement() {
    struct Panic;
    impl Read for Panic {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("injected child failure")
        }
    }
    let mut panic = Panic;
    let mut input = FramedImageInput::new(&mut panic);
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| input.read(&mut [0; 1]))).is_err()
    );
    assert!(input.read(&mut [0; 1]).is_err());
    let control = StorageReadControl::with_limit(16 << 10);
    let mut reader = framed_image_reader(
        ImageReader::new(Bytes(b"value"), &control).unwrap(),
        &control,
    )
    .unwrap();
    let required = control.memory().used();
    control.cancellation().cancel();
    assert!(reader.read(&mut [0; 1]).is_err());
    assert_eq!(control.memory().used(), 0);
    assert!(reader.read(&mut [0; 1]).is_err());
    let denied = StorageReadControl::with_limit(required - 1);
    let source = ImageReader::new(Bytes(b"value"), &denied).unwrap();
    assert!(framed_image_reader(source, &denied).is_err());
    assert_eq!(denied.memory().used(), 0);
}
