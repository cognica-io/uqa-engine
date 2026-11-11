//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

pub(super) fn retirement_source() -> Source {
    let mut rows = BTreeMap::new();
    for (index, name) in ["z", "a"].into_iter().enumerate() {
        let mut table = table(name);
        let mut security = crate::catalog::BoundRelationSecurity::owner(RoleIdentity::BOOTSTRAP);
        security.acl = Some(paths(1));
        for name in ["removed", "replaced"] {
            security.column_acls.insert(name.into(), paths(1));
        }
        table.security = RelationSecurityRow::Bound(security);
        rows.insert(
            format!("d/{index}").into_bytes(),
            serde_json::to_vec(&table).unwrap(),
        );
    }
    let mut source = Source {
        rows,
        control: StorageReadControl::with_limit(usize::MAX),
        reads: AtomicUsize::new(0),
        panic_acl: false,
    };
    // Cross the copy checkpoint while retaining every replacement branch and relation sorting.
    for (index, (column, acl)) in [
        (None, None),
        (Some("removed"), Some(Vec::new())),
        (Some("replaced"), Some(paths(65))),
        (Some("added"), Some(paths(1))),
        (Some("replaced"), Some(paths(1))),
        (None, Some(Vec::new())),
    ]
    .into_iter()
    .enumerate()
    {
        add_acl(&mut source, index, "z", column, acl);
    }
    source
}

#[test]
fn catalog_table_load_checks_duplicate_objects_after_each_original_owner_predicate() {
    fn accept(
        _: &dyn KeyValueRead,
        _: &TableSchema,
        _: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()
    }
    let mut source = source();
    source.panic_acl = true;
    let source = Arc::new(source);
    let control = StorageReadControl::with_limit(1 << 22);
    let limits = CatalogInputLimits {
        maximum_key_bytes: 256,
        maximum_value_bytes: 1 << 20,
    };
    let input = CatalogTableInput::new(
        source.clone(),
        (b"d/", definition),
        (b"a/", replacement),
        limits,
        &control,
    )
    .unwrap();
    let validation = CatalogTableValidation {
        read: source.clone(),
        validate: accept,
        limits,
    };
    let mut loader = CatalogTableLoad::new(
        input,
        CatalogTableOrder::AfterReplacements,
        Some(validation),
        &control,
    )
    .unwrap();
    assert_eq!(
        finish(&mut loader, 1).unwrap_err().to_string(),
        "duplicate table object identity"
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), 3);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(Arc::strong_count(&source), 1);
}

#[test]
fn catalog_table_load_preserves_legacy_and_orphan_replacement_rules() {
    for legacy in [false, true] {
        for matching in [false, true] {
            let mut source = source();
            source.rows.retain(|key, _| key.starts_with(b"d/"));
            let mut definition = table("z");
            if legacy {
                definition.security = RelationSecurityRow::legacy("owner");
            }
            source
                .rows
                .insert(b"d/0".to_vec(), serde_json::to_vec(&definition).unwrap());
            add_acl(
                &mut source,
                0,
                if matching { "z" } else { "absent" },
                Some("column"),
                None,
            );
            let control = StorageReadControl::with_limit(1 << 22);
            let mut loader = load(
                Arc::new(source),
                CatalogTableOrder::AfterReplacements,
                &control,
            )
            .unwrap();
            let result = finish(&mut loader, 1);
            if matching {
                let error = result.unwrap_err();
                assert_eq!(
                    error.to_string(),
                    if legacy {
                        "ACL tuple requires captured relation ownership"
                    } else {
                        "attribute ACL tuple has a null ACL"
                    }
                );
            } else {
                drop(result.unwrap());
            }
            assert_eq!(control.memory().used(), 0);
        }
    }
}
