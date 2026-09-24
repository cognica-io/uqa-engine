//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod failures;

#[derive(Default)]
pub(super) struct Faults {
    pub(super) fail_after_bytes: Option<usize>,
    pub(super) fail_truncate: bool,
    pub(super) written_bytes: u64,
    pub(super) read_blocks: usize,
}

impl Faults {
    pub(super) fn before_write(
        &mut self,
        file: &mut std::fs::File,
        bytes: &[u8],
    ) -> io::Result<()> {
        if let Some(remaining) = self.fail_after_bytes.as_mut() {
            if *remaining < bytes.len() {
                let count = *remaining;
                self.fail_after_bytes = None;
                file.write_all(&bytes[..count])?;
                self.written_bytes += count as u64;
                return Err(io::Error::other("injected partial temporary file write"));
            }
            *remaining -= bytes.len();
        }
        self.written_bytes += bytes.len() as u64;
        Ok(())
    }

    pub(super) fn before_truncate(&mut self) -> io::Result<()> {
        if std::mem::take(&mut self.fail_truncate) {
            return Err(io::Error::other("injected temporary file truncate failure"));
        }
        Ok(())
    }
}

#[test]
fn random_access_and_independent_readers_preserve_exact_bytes() {
    let mut file = TemporaryFile::new().unwrap();
    let mut expected = vec![b'a'; BLOCK_BYTES * 3 + 7];
    file.write_all(&expected).unwrap();
    file.seek(SeekFrom::Start((BLOCK_BYTES - 3) as u64))
        .unwrap();
    file.write_all(b"replacement").unwrap();
    expected[BLOCK_BYTES - 3..BLOCK_BYTES + 8].copy_from_slice(b"replacement");
    let mut first = file.reopen().unwrap();
    let mut second = file.reopen().unwrap();
    let mut prefix = [0; 13];
    first.read_exact(&mut prefix).unwrap();
    assert_eq!(prefix, expected[..13]);
    let mut all = Vec::new();
    second.read_to_end(&mut all).unwrap();
    assert_eq!(all, expected);
    let mut rest = Vec::new();
    first.read_to_end(&mut rest).unwrap();
    assert_eq!(rest, expected[13..]);
}

#[test]
fn sparse_growth_and_truncation_zero_newly_visible_bytes() {
    let mut file = TemporaryFile::new().unwrap();
    file.seek(SeekFrom::Start((BLOCK_BYTES + 5) as u64))
        .unwrap();
    file.write_all(b"secret").unwrap();
    let mut expected = vec![0; BLOCK_BYTES + 5];
    expected.extend_from_slice(b"secret");
    let mut bytes = Vec::new();
    file.reopen().unwrap().read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, expected);
    file.set_len((BLOCK_BYTES + 7) as u64).unwrap();
    file.set_len((BLOCK_BYTES * 2 + 3) as u64).unwrap();
    expected.truncate(BLOCK_BYTES + 7);
    expected.resize(BLOCK_BYTES * 2 + 3, 0);
    bytes.clear();
    file.reopen().unwrap().read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, expected);
    file.set_len(0).unwrap();
    file.set_len(23).unwrap();
    bytes.clear();
    file.reopen().unwrap().read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, vec![0; 23]);
}

#[test]
fn ciphertext_hides_payload_and_last_reader_removes_the_file() {
    const SECRET: &[u8] = b"confidential-retained-query-and-mutation-marker";
    let mut file = TemporaryFile::new().unwrap();
    file.write_all(SECRET).unwrap();
    file.flush().unwrap();
    let path = file.path().to_owned();
    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes.windows(SECRET.len()).any(|bytes| bytes == SECRET));
    let mut reader = file.reopen().unwrap();
    drop(file);
    assert!(path.exists());
    let mut restored = Vec::new();
    reader.read_to_end(&mut restored).unwrap();
    assert_eq!(restored, SECRET);
    drop(reader);
    assert!(!path.exists());
}

#[test]
fn modified_truncated_and_transplanted_blocks_are_rejected() {
    for damage in 0..6 {
        let mut file = TemporaryFile::new().unwrap();
        file.write_all(&vec![b'a'; BLOCK_BYTES * 2]).unwrap();
        let mut bytes = std::fs::read(file.path()).unwrap();
        match damage {
            0 => bytes[1 + NONCE_BYTES + 3] ^= 1,
            1 => {
                bytes.pop();
            }
            2 => {
                let second = bytes[RECORD_BYTES..].to_vec();
                bytes[..RECORD_BYTES].copy_from_slice(&second);
            }
            3 => {
                let mut other = TemporaryFile::new().unwrap();
                other.write_all(&vec![b'b'; BLOCK_BYTES * 2]).unwrap();
                bytes = std::fs::read(other.path()).unwrap();
            }
            4 => bytes[0] = 2,
            _ => bytes[0] = 1,
        }
        std::fs::write(file.path(), bytes).unwrap();
        let result = file.reopen().and_then(|mut reader| {
            let mut output = [0; 1];
            reader.read_exact(&mut output)
        });
        assert!(result.is_err(), "damage {damage} was accepted");
    }
}

#[test]
fn offset_sized_blocks_append_one_record_without_redecrypting_earlier_offsets() {
    const OFFSETS: u64 = 257;
    let mut file = BlockTemporaryFile::<8>::new().unwrap();
    for value in 0..OFFSETS {
        file.write_all(&value.to_le_bytes()).unwrap();
    }
    assert_eq!(file.owner.lock().faults.read_blocks, 0);
    // Each append writes exactly nonce + eight ciphertext bytes + tag + selector. The inactive slot is reserved, and no prior offset block is rewritten.
    assert_eq!(file.owner.lock().faults.written_bytes, OFFSETS * 49);
    assert_eq!(std::fs::metadata(file.path()).unwrap().len(), OFFSETS * 97);
    let mut reader = file.reopen().unwrap();
    for value in 0..OFFSETS {
        let mut bytes = [0; 8];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(u64::from_le_bytes(bytes), value);
    }
}

#[test]
fn vectored_records_share_one_block_publication_and_preserve_slice_order() {
    let mut file = TemporaryFile::new().unwrap();
    assert_eq!(
        file.write_vectored(&[
            IoSlice::new(b"length"),
            IoSlice::new(b""),
            IoSlice::new(b"payload")
        ])
        .unwrap(),
        13
    );
    assert_eq!(
        file.owner.lock().faults.written_bytes,
        (NONCE_BYTES + BLOCK_BYTES + TAG_BYTES + 1) as u64
    );
    let mut bytes = Vec::new();
    file.reopen().unwrap().read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"lengthpayload");

    let mut small = BlockTemporaryFile::<8>::new().unwrap();
    small.write_all(b"prefix-").unwrap();
    let mut slices = [IoSlice::new(b"one"), IoSlice::new(b"two")];
    let mut remaining = &mut slices[..];
    while !remaining.is_empty() {
        let written = small.write_vectored(remaining).unwrap();
        assert!(written > 0);
        IoSlice::advance_slices(&mut remaining, written);
    }
    bytes.clear();
    small.reopen().unwrap().read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"prefix-onetwo");
    assert!(BlockTemporaryFile::<0>::new().is_err());
    assert!(BlockTemporaryFile::<{ BLOCK_BYTES + 1 }>::new().is_err());
}

#[test]
fn complete_vectored_records_skip_empty_slices_and_publish_each_block_once() {
    let mut file = BlockTemporaryFile::<8>::new().unwrap();
    file.write_all(b"prefix-").unwrap();
    let written = file.owner.lock().faults.written_bytes;
    file.write_all_vectored(&mut [
        IoSlice::new(b""),
        IoSlice::new(b"one"),
        IoSlice::new(b""),
        IoSlice::new(b"two-three"),
        IoSlice::new(b""),
    ])
    .unwrap();
    assert_eq!(
        file.owner.lock().faults.written_bytes - written,
        3 * (NONCE_BYTES + 8 + TAG_BYTES + 1) as u64
    );
    let mut bytes = Vec::new();
    file.reopen().unwrap().read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"prefix-onetwo-three");
    let written = file.owner.lock().faults.written_bytes;
    file.write_all_vectored(&mut []).unwrap();
    file.write_all_vectored(&mut [IoSlice::new(b""), IoSlice::new(b"")])
        .unwrap();
    assert_eq!(file.owner.lock().faults.written_bytes, written);
    assert_eq!(file.stream_position().unwrap(), bytes.len() as u64);
}

#[test]
fn impossible_offsets_fail_without_changing_existing_data() {
    let mut file = TemporaryFile::new().unwrap();
    file.write_all(b"retained").unwrap();
    assert!(file.set_len(u64::MAX).is_err());
    file.seek(SeekFrom::Start(u64::MAX)).unwrap();
    assert!(file.write_all(b"rejected").is_err());
    assert!(file.seek(SeekFrom::Start(0)).is_ok());
    assert!(file.seek(SeekFrom::Current(-1)).is_err());
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"retained");
}
