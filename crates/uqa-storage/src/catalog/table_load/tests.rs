//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
mod predicates;
use crate::{
    catalog::{
        input::table,
        relation_acl::{self, RelationAclRecords, RelationAclTuple},
        table_input::CatalogTableAcl,
    },
    key_value::{KeyValueReadRevision, RecordKeyReadVisitor},
    read_control::{KeyValueReadVisitor, ValueReadVisitor},
    RelationIdentity, RelationSecurityRow,
};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicUsize, Ordering},
};
use uqa_core::{
    catalog_acl::TablePrivileges,
    catalog_role::{BoundAclEntry, RoleIdentity},
};

struct Source {
    rows: BTreeMap<Vec<u8>, Vec<u8>>,
    control: StorageReadControl,
    reads: AtomicUsize,
    panic_acl: bool,
}

impl KeyValueRead for Source {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        panic!("original view is retained")
    }
    fn visit_value(&self, _: &[u8], _: &mut ValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("bounded values required")
    }
    fn visit_prefix(&self, _: &[u8], _: &mut KeyValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("bounded record input required")
    }
    fn visit_record_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut RecordKeyReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        assert_eq!(limit, 1);
        if let Some((key, _)) = self.rows.iter().find(|(key, _)| {
            key.starts_with(prefix) && after.is_none_or(|after| key.as_slice() > after)
        }) {
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
        assert!(
            !(self.panic_acl && key.starts_with(b"a/")),
            "source ACL panic"
        );
        self.reads.fetch_add(1, Ordering::SeqCst);
        let value = self.rows.get(key).map(Vec::as_slice);
        if let Some(value) = value {
            control.check_value_size(value.len(), maximum)?;
        }
        visit(value)
    }
}

fn definition(
    _: &[u8],
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<TableSchema>>> {
    table::row(bytes, control).map(Some)
}

fn replacement(
    _: &[u8],
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogTableAcl>>> {
    let split = bytes.iter().position(|byte| *byte == b'\n').unwrap();
    CatalogTableAcl::decode(
        std::str::from_utf8(&bytes[..split]).unwrap(),
        &bytes[split + 1..],
        control,
    )
    .map(Some)
}

fn paths(count: usize) -> Vec<BoundAclEntry<TablePrivileges>> {
    (0..count)
        .map(|_| BoundAclEntry {
            role: None,
            grantor: RoleIdentity::BOOTSTRAP,
            privileges: TablePrivileges::default(),
            grant_options: TablePrivileges::default(),
        })
        .collect()
}

fn table(name: &str) -> TableSchema {
    let mut security = crate::catalog::BoundRelationSecurity::owner(RoleIdentity::BOOTSTRAP);
    security.acl = Some(paths(128));
    security.column_acls = [
        ("removed".to_owned(), paths(128)),
        ("replaced".to_owned(), paths(128)),
    ]
    .into_iter()
    .collect();
    TableSchema {
        relation: RelationIdentity::new("public", name),
        security: RelationSecurityRow::Bound(security),
        object_id: [1; 16],
        storage_generation: [2; 16],
        analyzer_json: "{}".into(),
        fts_fields: vec!["text".into()],
        vector_fields: Vec::new(),
        columns_json: "[]".into(),
        constraints_json: "[]".into(),
    }
}

fn source() -> Source {
    let mut rows = BTreeMap::new();
    for (index, name) in ["z", "a", "z"].iter().enumerate() {
        rows.insert(
            format!("d/{index}").into_bytes(),
            serde_json::to_vec(&table(name)).unwrap(),
        );
    }
    let mut source = Source {
        rows,
        control: StorageReadControl::with_limit(usize::MAX),
        reads: AtomicUsize::new(0),
        panic_acl: false,
    };
    for (index, (column, acl)) in [
        (None, None),
        (Some("removed"), Some(Vec::new())),
        (Some("replaced"), Some(paths(256))),
        (Some("added"), Some(paths(32))),
        (Some("replaced"), Some(paths(64))),
        (None, Some(Vec::new())),
    ]
    .into_iter()
    .enumerate()
    {
        add_acl(&mut source, index, "z", column, acl);
    }
    source
}

fn add_acl(
    source: &mut Source,
    index: usize,
    name: &str,
    column: Option<&str>,
    acl: Option<Vec<BoundAclEntry<TablePrivileges>>>,
) {
    let key = relation_acl::key(&RelationIdentity::new("public", name), column);
    let tuple = RelationAclTuple::new(acl).unwrap();
    source.rows.insert(
        format!("a/{index:02}").into_bytes(),
        format!("{key}\n{}", serde_json::to_string(&tuple).unwrap()).into_bytes(),
    );
}

fn expected(source: &Source) -> Vec<TableSchema> {
    let mut tables = Vec::new();
    let mut records = RelationAclRecords::default();
    for (key, bytes) in &source.rows {
        if key.starts_with(b"d/") {
            tables.push(serde_json::from_slice::<TableSchema>(bytes).unwrap());
        } else {
            let split = bytes.iter().position(|byte| *byte == b'\n').unwrap();
            records
                .insert(
                    std::str::from_utf8(&bytes[..split]).unwrap(),
                    &bytes[split + 1..],
                )
                .unwrap();
        }
    }
    for table in &mut tables {
        records.apply(&table.relation, &mut table.security).unwrap();
    }
    tables.sort_by(|a, b| a.relation.cmp(&b.relation));
    tables
}

fn load(
    source: Arc<Source>,
    order: CatalogTableOrder,
    control: &StorageReadControl,
) -> StorageBackendResult<CatalogTableLoad> {
    let input = CatalogTableInput::new(
        source,
        (b"d/", definition),
        (b"a/", replacement),
        CatalogInputLimits {
            maximum_key_bytes: 256,
            maximum_value_bytes: 1 << 20,
        },
        control,
    )?;
    CatalogTableLoad::new(input, order, None, control)
}

fn finish(input: &mut CatalogTableLoad, steps: usize) -> StorageBackendResult<CatalogTables> {
    for _ in 0..100_000 {
        if let Some(tables) = input.advance(steps)? {
            return Ok(tables);
        }
    }
    panic!("finite catalog must finish")
}

#[test]
fn catalog_table_load_preserves_replacement_order_duplicate_relations_and_exact_live_allocations() {
    let source = Arc::new(source());
    let expected = expected(&source);
    for order in [
        CatalogTableOrder::BeforeReplacements,
        CatalogTableOrder::AfterReplacements,
    ] {
        let control = StorageReadControl::with_limit(1 << 22);
        let mut output = None;
        let allocation = allocation_counter::measure(|| {
            let mut input = load(source.clone(), order, &control).unwrap();
            for _ in 0..100_000 {
                let before = source.reads.load(Ordering::SeqCst);
                output = input.advance(1).unwrap();
                assert!(source.reads.load(Ordering::SeqCst) - before <= 1);
                if output.is_some() {
                    break;
                }
            }
            assert!(input.advance(1).is_err());
        });
        assert_eq!(allocation.bytes_current as usize, control.memory().used());
        control.cancellation().cancel();
        let output = output.unwrap();
        assert_eq!(output.len(), expected.len());
        for (actual, expected) in output.iter().zip(&expected) {
            assert_eq!(
                serde_json::to_value(&**actual).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
            assert_eq!(actual.security, expected.security);
        }
        drop(output);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn catalog_table_load_retires_views_and_payloads_on_every_partial_drop_and_cancellation() {
    let source = Arc::new(predicates::retirement_source());
    let control = StorageReadControl::with_limit(1 << 22);
    let mut input = load(
        source.clone(),
        CatalogTableOrder::AfterReplacements,
        &control,
    )
    .unwrap();
    let mut steps = 0;
    while input.advance(1).unwrap().is_none() {
        steps += 1;
    }
    assert_eq!(control.memory().used(), 0);
    for stop in 0..=steps {
        for cancel in [false, true] {
            let control = StorageReadControl::with_limit(1 << 22);
            let mut input = load(
                source.clone(),
                CatalogTableOrder::AfterReplacements,
                &control,
            )
            .unwrap();
            for _ in 0..stop {
                assert!(input.advance(1).unwrap().is_none());
            }
            if cancel {
                control.cancellation().cancel();
                assert!(input.advance(1).is_err());
            }
            drop(input);
            assert_eq!(control.memory().used(), 0, "step {stop}, cancel {cancel}");
            assert_eq!(Arc::strong_count(&source), 1);
        }
    }
}

#[test]
fn catalog_table_load_denies_peak_shortfall_and_preserves_late_input_error_precedence() {
    let source = Arc::new(source());
    let control = StorageReadControl::with_limit(1 << 22);
    drop(
        finish(
            &mut load(
                source.clone(),
                CatalogTableOrder::BeforeReplacements,
                &control,
            )
            .unwrap(),
            64,
        )
        .unwrap(),
    );
    let peak = control.memory().peak();
    for limit in [0, 256, peak - 1, peak] {
        let control = StorageReadControl::with_limit(limit);
        let result = load(
            source.clone(),
            CatalogTableOrder::BeforeReplacements,
            &control,
        )
        .and_then(|mut input| finish(&mut input, 64));
        assert_eq!(result.is_ok(), limit == peak);
        drop(result);
        assert_eq!(control.memory().used(), 0);
    }
    let mut source = self::source();
    add_acl(&mut source, 7, "z", Some("invalid-null"), None);
    source.rows.insert(
        b"a/99".to_vec(),
        b"uqa.relation_acl.v1:{\"schema\":\"public\",\"name\":\"z\"}:null\ninvalid".to_vec(),
    );
    let control = StorageReadControl::with_limit(1 << 22);
    let error = finish(
        &mut load(
            Arc::new(source),
            CatalogTableOrder::AfterReplacements,
            &control,
        )
        .unwrap(),
        64,
    )
    .unwrap_err();
    assert!(matches!(error, StorageBackendError::Serde(_)), "{error}");
    drop(error);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn catalog_table_load_owner_failure_precedes_acl_reads_and_unwinding_retires_the_view() {
    fn reject(
        _: &dyn KeyValueRead,
        _: &TableSchema,
        _: CatalogInputLimits,
        _: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        Err(StorageBackendError::Other("original owner failure".into()))
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
        validate: reject,
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
        finish(&mut loader, 64).unwrap_err().to_string(),
        "original owner failure"
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), 3);
    assert_eq!(control.memory().used(), 0);
    let mut loader = load(
        source.clone(),
        CatalogTableOrder::AfterReplacements,
        &control,
    )
    .unwrap();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| finish(&mut loader, 64))).is_err()
    );
    assert_eq!(control.memory().used(), 0);
    assert_eq!(Arc::strong_count(&source), 1);
    assert!(loader.advance(1).is_err());
}
