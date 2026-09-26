//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generation reservations use one nontransactional watermark per physical history database.
//!
//! A provider upgrading per-index reservations must fold their maximum into this namespace and remove only their exact namespace rows in the same atomic format upgrade. The format fence must exclude predecessor allocators before admitting new allocations. Existing generation records and unrelated identifier domains remain unchanged.

use super::keys::ROOT;

/// Autonomous generation allocation domain, independent of table/index churn and restored data identities.
pub const NAMESPACE: &[u8] = b"\0uqa-diskann-generations-v1\0";

/// Exact predecessor domain before its data identity and two physical handles.
pub const LEGACY_PREFIX: [u8; ROOT.len() + 1] = {
    let mut prefix = [0; ROOT.len() + 1];
    let mut index = 0;
    while index < ROOT.len() {
        prefix[index] = ROOT[index];
        index += 1;
    }
    prefix[ROOT.len()] = 1;
    prefix
};

pub const LEGACY_NAMESPACE_BYTES: usize = LEGACY_PREFIX.len() + 16 + 8 + 8;

/// Upper bound of the predecessor prefix's byte range, excluded by a provider cursor.
pub const LEGACY_END: [u8; LEGACY_PREFIX.len()] = {
    let mut prefix = LEGACY_PREFIX;
    prefix[ROOT.len()] = 2;
    prefix
};

pub fn is_legacy_namespace(namespace: &[u8]) -> bool {
    namespace.len() == LEGACY_NAMESPACE_BYTES && namespace.starts_with(&LEGACY_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diskann_index::format::DiskANNGeneration;
    use crate::key_value::diskann::keys::Keys;

    #[test]
    fn diskann_identifier_domains_select_only_complete_predecessor_namespaces() {
        let keys = Keys::new(DiskANNGeneration::new([9; 16], 11, 12, 13).unwrap());
        let legacy = keys.legacy_allocation_namespace();
        assert!(is_legacy_namespace(legacy));
        assert!(legacy >= LEGACY_PREFIX.as_slice() && legacy < LEGACY_END.as_slice());
        for unrelated in [
            &legacy[..legacy.len() - 1],
            keys.prefix(),
            NAMESPACE,
            LEGACY_PREFIX.as_slice(),
            LEGACY_END.as_slice(),
            b"ordinary-reservations",
        ] {
            assert!(!is_legacy_namespace(unrelated));
        }
    }
}
