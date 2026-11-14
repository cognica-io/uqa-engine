//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::byte_windows::{assert_retirement, read_windowed, WINDOWS};
use super::*;

#[test]
fn sequence_authority_input_bounds_acl_fields_and_retires_partial_catalogs() {
    let source = singleton(Some(vec![
        BoundAclEntry {
            role: None,
            grantor: RoleIdentity::BOOTSTRAP,
            privileges: SequencePrivileges::ALL,
            grant_options: SequencePrivileges::default(),
        };
        257
    ]));
    let bytes = encode(&source);
    let token = CancellationToken::new();
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(1 << 20);
        let restored = read_windowed(
            SequenceSecurities::image_input(&memory, &memory, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert_eq!(restored, source);
        assert_eq!(encode(&restored), bytes);
        drop(restored);
        assert_eq!(memory.used(), 0);
    }
    assert_retirement(&bytes, SequenceSecurities::image_input);
}
