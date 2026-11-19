//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::{
        input::{
            strings::{fields, take},
            CatalogInputLimits,
        },
        relation_acl::{self, RelationAclTuple},
        table_input::CatalogTableAcl,
    },
    key_value::{KeyValueRead, KeyValueReadRevision, RecordKeyReadVisitor},
    read_control::{KeyValueReadVisitor, ValueReadVisitor},
    RelationIdentity, RelationSecurityRow,
};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
        Arc,
    },
};

struct Source {
    rows: BTreeMap<Vec<u8>, Vec<u8>>,
    control: StorageReadControl,
    visits: Arc<AtomicUsize>,
}

impl KeyValueRead for Source {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        panic!("foreign table input must not recompute its view")
    }
    fn visit_value(&self, _: &[u8], _: &mut ValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("foreign table values require bounded reads")
    }
    fn visit_prefix(&self, _: &[u8], _: &mut KeyValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("foreign table input must not collect provider rows")
    }
    fn visit_record_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut RecordKeyReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        assert_eq!(limit, 1);
        control.check()?;
        if let Some(key) = self
            .rows
            .keys()
            .find(|key| key.starts_with(prefix) && after.is_none_or(|after| key.as_slice() > after))
        {
            self.visits.fetch_add(1, AtomicOrdering::SeqCst);
            visit(key, true)?;
        }
        Ok(())
    }
    fn visit_value_bounded(
        &self,
        key: &[u8],
        maximum: usize,
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        let value = self.rows.get(key).map(Vec::as_slice);
        if let Some(value) = value {
            control.check_value_size(value.len(), maximum)?;
        }
        visit(value)
    }
}

fn decode(
    _: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<ForeignTableRow>>> {
    let [schema, name, server] = fields(
        value,
        [("schema", false), ("name", false), ("server", false)],
        control,
    )?
    .map(Option::unwrap);
    let mut memory = control.memory().empty_reservation();
    Ok(Some(Budgeted::new(
        ForeignTableRow {
            relation: RelationIdentity {
                schema: take(schema, &mut memory),
                name: take(name, &mut memory),
            },
            security: RelationSecurityRow::bootstrap(),
            server_name: take(server, &mut memory),
            columns_json: String::new(),
            options_json: String::new(),
        },
        memory,
    )))
}

fn acl(
    _: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogTableAcl>>> {
    let split = value.iter().position(|byte| *byte == b'\n').unwrap();
    CatalogTableAcl::decode(
        std::str::from_utf8(&value[..split]).unwrap(),
        &value[split + 1..],
        control,
    )
    .map(Some)
}

fn fixture(control: &StorageReadControl) -> Arc<Source> {
    let mut rows: BTreeMap<Vec<u8>, Vec<u8>> = [
        (b"d/0".to_vec(), br#"["public","z","first"]"#.to_vec()),
        (b"d/1".to_vec(), br#"["public","a","second"]"#.to_vec()),
        (b"d/2".to_vec(), br#"["public","a","third"]"#.to_vec()),
    ]
    .into();
    let key = relation_acl::key(&RelationIdentity::new("public", "z"), None);
    let tuple = RelationAclTuple::new(Some(Vec::new())).unwrap();
    rows.insert(
        b"a/0".to_vec(),
        format!("{key}\n{}", serde_json::to_string(&tuple).unwrap()).into_bytes(),
    );
    Arc::new(Source {
        rows,
        control: control.clone(),
        visits: Arc::default(),
    })
}

fn cursor(source: Arc<Source>, control: &StorageReadControl) -> CatalogTableInput<ForeignTableRow> {
    CatalogTableInput::new(
        source,
        (b"d/", decode),
        (b"a/", acl),
        CatalogInputLimits {
            maximum_key_bytes: 4096,
            maximum_value_bytes: 65536,
        },
        control,
    )
    .unwrap()
}

fn finish(
    input: &mut CatalogForeignTableLoad,
) -> StorageBackendResult<(CatalogForeignTables, usize)> {
    for step in 1..1024 {
        if let Some(rows) = input.advance(1)? {
            return Ok((rows, step));
        }
    }
    panic!("foreign input stopped progressing");
}

#[test]
fn complete_foreign_input_preserves_physical_and_stable_relation_order_with_acl_replacement() {
    for order in [
        CatalogForeignTableOrder::Physical,
        CatalogForeignTableOrder::Relation,
    ] {
        let control = StorageReadControl::with_limit(1 << 20);
        let source = fixture(&control);
        let weak = Arc::downgrade(&source);
        let visits = Arc::clone(&source.visits);
        let mut input =
            CatalogForeignTableLoad::new(cursor(source, &control), order, &control).unwrap();
        let rows = loop {
            let before = visits.load(AtomicOrdering::SeqCst);
            let result = input.advance(1).unwrap();
            assert!(visits.load(AtomicOrdering::SeqCst) <= before + 1);
            if let Some(rows) = result {
                break rows;
            }
        };
        let expected = match order {
            CatalogForeignTableOrder::Physical => ["first", "second", "third"],
            CatalogForeignTableOrder::Relation => ["second", "third", "first"],
        };
        assert_eq!(
            rows.iter()
                .map(|row| row.server_name.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        let RelationSecurityRow::Bound(security) = &rows
            .iter()
            .find(|row| row.relation.name == "z")
            .unwrap()
            .security
        else {
            panic!("bound authority")
        };
        assert_eq!(security.acl, Some(Vec::new()));
        assert!(security.acl_revisions.relation.is_some());
        assert_eq!(visits.load(AtomicOrdering::SeqCst), 4);
        assert!(weak.upgrade().is_none());
        assert!(input.advance(1).is_err());
        drop((rows, input));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn complete_foreign_input_rejects_late_definitions_and_acl_errors_before_returning_rows() {
    for key in [b"d/3".as_slice(), b"a/1".as_slice()] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut source = fixture(&control);
        Arc::get_mut(&mut source).unwrap().rows.insert(
            key.to_vec(),
            if key[0] == b'a' {
                b"broken\n{}".to_vec()
            } else {
                b"{".to_vec()
            },
        );
        let weak = Arc::downgrade(&source);
        let mut input = CatalogForeignTableLoad::new(
            cursor(source, &control),
            CatalogForeignTableOrder::Relation,
            &control,
        )
        .unwrap();
        assert!(finish(&mut input).is_err());
        assert!(input.advance(1).is_err());
        assert!(weak.upgrade().is_none());
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn complete_foreign_input_retires_each_terminal_prefix_and_rejects_changed_controls() {
    let control = StorageReadControl::with_limit(1 << 20);
    let source = fixture(&control);
    let mut input = CatalogForeignTableLoad::new(
        cursor(Arc::clone(&source), &control),
        CatalogForeignTableOrder::Relation,
        &control,
    )
    .unwrap();
    let (rows, steps) = finish(&mut input).unwrap();
    drop((rows, input));
    for prefix in 0..steps - 1 {
        for terminal in ["cancel", "zero", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(1 << 20);
            let mut input = CatalogForeignTableLoad::new(
                cursor(Arc::clone(&source), &control),
                CatalogForeignTableOrder::Relation,
                &control,
            )
            .unwrap();
            for _ in 0..prefix {
                assert!(input.advance(1).unwrap().is_none());
            }
            match terminal {
                "cancel" => {
                    control.cancellation().cancel();
                    assert!(input.advance(1).is_err());
                }
                "zero" => assert!(input.advance(0).is_err()),
                "unwind" => {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _input = input;
                        panic!("abandon foreign input");
                    }));
                    assert_eq!(control.memory().used(), 0);
                    continue;
                }
                "drop" => (),
                _ => unreachable!(),
            }
            drop(input);
            assert_eq!(control.memory().used(), 0);
        }
    }
    let changed = StorageReadControl::with_limit(1 << 20);
    assert!(CatalogForeignTableLoad::new(
        cursor(source, &control),
        CatalogForeignTableOrder::Relation,
        &changed
    )
    .is_err());
    assert_eq!(control.memory().used(), 0);
    assert_eq!(changed.memory().used(), 0);
}
