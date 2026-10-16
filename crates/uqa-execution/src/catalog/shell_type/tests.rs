//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A shell restores exactly one claimed identity, without an array or an implicit format fallback.

use super::*;
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

#[test]
fn shell_records_require_one_exact_oid_claim_and_a_known_format() {
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    let shell = StoredShellType {
        object_id: [1; 16],
        oid: 20_000,
        identity: RelationIdentity::new("public", "pending"),
        owner: RoleIdentity::BOOTSTRAP,
        usage_acl: None,
    };
    let registry = ShellRegistry::from([("public.pending".into(), shell)]);
    for corruption in 0..5 {
        let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
        assert!(restore(&catalog, &roles).unwrap().is_empty());
        FORMAT
            .persist(&catalog, &ShellRegistry::new(), &registry)
            .unwrap();
        let restored = restore(&catalog, &roles).unwrap();
        assert_eq!(restored["public.pending"].oid, 20_000);
        assert_eq!(restored["public.pending"].object_id, [1; 16]);
        assert_eq!(
            catalog
                .metadata_with_prefix(FORMAT.oid_prefix)
                .unwrap()
                .len(),
            1
        );
        match corruption {
            0 => catalog.delete_metadata(&format!("{}20000", FORMAT.oid_prefix)),
            1 => catalog.set_metadata(&format!("{}20001", FORMAT.oid_prefix), "public.pending"),
            2 => catalog.set_metadata(&format!("{}20000", FORMAT.oid_prefix), "public.other"),
            3 => catalog.delete_metadata(FORMAT.format_key),
            _ => catalog.set_metadata(FORMAT.format_key, "{\"shell_catalog_format\":2}"),
        }
        .unwrap();
        assert!(
            restore(&catalog, &roles).is_err(),
            "corruption {corruption}"
        );
    }
}
