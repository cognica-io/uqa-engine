//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::TableMetadata;
use crate::catalog::security::{table_binding::BoundTableAclEntry, BoundTableSecurity};
use uqa_core::{
    catalog_acl::TablePrivileges,
    catalog_role::RoleIdentity,
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

fn original() -> BoundTableSecurity {
    let mut value = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
    let entry = BoundTableAclEntry {
        role: None,
        grantor: RoleIdentity::BOOTSTRAP,
        privileges: TablePrivileges {
            select: true,
            ..TablePrivileges::default()
        },
        grant_options: TablePrivileges::default(),
    };
    value.acl = Some(vec![entry.clone()]);
    value.column_acls.insert("body".into(), vec![entry]);
    value.column_acls.insert("empty".into(), vec![]);
    value.acl_revisions.relation = Some([1; 16]);
    value.acl_revisions.columns.insert("body".into(), [2; 16]);
    value
}

fn image(value: &BoundTableSecurity) -> Vec<u8> {
    let token = CancellationToken::new();
    let mut reader = value.image_reader(&token).unwrap();
    let mut bytes = vec![];
    let mut buffer = [0; 19];
    loop {
        let n = reader.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    bytes
}

#[test]
fn imported_authority_metadata_keeps_native_acl_buffers_and_nested_node_admission() {
    let expected = original();
    let bytes = image(&expected);
    let input = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let decoded = BoundTableSecurity::read_image(&mut bytes.as_slice(), &input, &token).unwrap();
    let original_name = decoded.column_acls.keys().next().unwrap().as_ptr();
    let original_acl = decoded.acl.as_ref().unwrap().as_ptr();
    let native = TableMetadata::from_admitted(decoded, &token).unwrap();
    assert_eq!(
        native.column_acls.keys().next().unwrap().as_ptr(),
        original_name
    );
    assert_eq!(native.acl.as_ref().unwrap().as_ptr(), original_acl);
    let retained_bytes = input.used();
    super::capture_bytes(&native, retained_bytes);
    let reader = native.snapshot();
    let mut live = native.clone();
    {
        let mut changed = TableMetadata::write(&mut live);
        changed.acl = None;
        changed.remove_column_acl("body");
    }
    assert!(!reader.shares_definition(&live.snapshot()));
    assert_eq!(image(&reader), bytes);
    assert_eq!(*reader, expected);
    drop((native, live));
    assert_eq!(input.used(), retained_bytes);
    drop(reader);
    assert_eq!(input.used(), 0);
}

#[test]
fn imported_authority_metadata_rejects_header_exhaustion_and_cancelled_adoption() {
    let bytes = image(&original());
    for cancel in [false, true] {
        let input = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let decoded =
            BoundTableSecurity::read_image(&mut bytes.as_slice(), &input, &token).unwrap();
        let blocker = input.reserve(input.limit() - input.used()).unwrap();
        if cancel {
            token.cancel();
        }
        assert!(TableMetadata::from_admitted(decoded, &token).is_err());
        assert_eq!(input.used(), blocker.bytes());
        drop(blocker);
        assert_eq!(input.used(), 0);
    }
}

#[test]
fn authority_metadata_mutation_after_input_cancellation_recounts_native_maps() {
    let input = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let decoded =
        BoundTableSecurity::read_image(&mut image(&original()).as_slice(), &input, &token).unwrap();
    let mut native = TableMetadata::from_admitted(decoded, &token).unwrap();
    token.cancel();
    {
        let mut writer = TableMetadata::write(&mut native);
        writer.remove_column_acl("body");
        writer.acl = None;
    }
    assert_eq!(input.used(), 0);
    let control = ProductionControl::uncontrolled();
    let copied = native.capture_with_control(&control).unwrap();
    assert!(copied.acl.is_none());
    assert!(!copied.column_acls.contains_key("body"));
    assert_eq!(copied.acl_revisions.relation, Some([1; 16]));
}
