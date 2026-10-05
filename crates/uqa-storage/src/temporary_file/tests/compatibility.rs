//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn authenticated_blocks_read_ciphertext_from_the_previous_cipher_version() {
    // Produced with chacha20poly1305 0.10.1 at 94d825d5e using the key, nonce, block, length and plaintext below. The 513-byte payload covers eight ChaCha blocks and a partial tail.
    const LEGACY: &[u8; TAG_BYTES + 513] = include_bytes!("fixtures/chacha20poly1305-0.10.bin");
    const BLOCK: u64 = 3;
    const NONCE: [u8; NONCE_BYTES] = [0x24; NONCE_BYTES];
    let expected: Vec<u8> = (0..513)
        .map(|i| (i as u8).wrapping_mul(17).wrapping_add(3))
        .collect();
    let mut file = TemporaryFile::new().unwrap();
    {
        let mut owner = file.owner.lock();
        owner.cipher = XChaCha20Poly1305::new((&[0x42_u8; 32]).into());
        owner.length = BLOCK * BLOCK_BYTES as u64 + expected.len() as u64;
        let physical_len = TemporaryFile::physical_len_for(owner.length).unwrap();
        let physical = owner.file.as_file_mut();
        physical.set_len(physical_len).unwrap();
        physical
            .seek(SeekFrom::Start(
                slot_offset::<BLOCK_BYTES>(BLOCK, 0).unwrap(),
            ))
            .unwrap();
        physical.write_all(&NONCE).unwrap();
        physical.write_all(&513_u16.to_le_bytes()).unwrap();
        physical.write_all(LEGACY).unwrap();
    }
    file.seek(SeekFrom::Start(BLOCK * BLOCK_BYTES as u64))
        .unwrap();
    let mut actual = Vec::new();
    file.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, expected);

    let owner = file.owner.lock();
    let tag = owner
        .cipher
        .encrypt_inout_detached(
            (&NONCE).into(),
            &block_aad(BLOCK, 513),
            actual.as_mut_slice().into(),
        )
        .unwrap();
    assert_eq!(tag.as_slice(), &LEGACY[..TAG_BYTES]);
    assert_eq!(actual, LEGACY[TAG_BYTES..]);
}
