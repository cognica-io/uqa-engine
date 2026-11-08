//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ast::RelationPersistence;

mod resources;

fn row(name: &str, definition: Option<&str>) -> CatalogIndexRow {
    CatalogIndexRow {
        relation: RelationIdentity::new("Quoted.Schema", name),
        index_type: "btree".into(),
        table_name: "\"Quoted.Schema\".\"Table.Name\"".into(),
        columns_json: "[ \"id\" ]".into(),
        parameters_json: "{ \"x\": 1.00, \"x\": 2 }".into(),
        definition_json: definition.map(str::to_owned),
    }
}

fn wire(rows: &[CatalogIndexRow]) -> Vec<u8> {
    let mut output = b"UQAIDX01".to_vec();
    output.extend_from_slice(&(rows.len() as u64).to_le_bytes());
    for row in rows {
        for value in [
            &row.relation.schema,
            &row.relation.name,
            &row.index_type,
            &row.table_name,
            &row.columns_json,
            &row.parameters_json,
        ] {
            output.extend_from_slice(&(value.len() as u64).to_le_bytes());
            output.extend_from_slice(value.as_bytes());
        }
        match &row.definition_json {
            None => output.push(0),
            Some(value) => {
                output.push(1);
                output.extend_from_slice(&(value.len() as u64).to_le_bytes());
                output.extend_from_slice(value.as_bytes());
            }
        }
    }
    output
}

fn bytes(captured: &CapturedCatalogIndexes) -> Vec<u8> {
    let control = StorageReadControl::with_limit(4096);
    let mut reader = captured.image_reader(&control).unwrap();
    let mut bytes = Vec::new();
    let mut part = [0; 17];
    loop {
        let count = reader.read(&mut part).unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&part[..count]);
    }
    assert_eq!(control.memory().used(), 0);
    bytes
}

#[test]
fn native_index_images_preserve_exact_encodings_and_durable_membership() {
    let rows = [
        row("a", None),
        row("b", Some("")),
        row("c", Some("{ \"x\": 1.00, \"x\": 2 }")),
    ];
    let native = MemoryBudget::new(1 << 20);
    let restored = MemoryBudget::new(1 << 20);
    let control = StorageReadControl::with_limit(1 << 20);
    let mut source = CatalogIndexes::with_budget(&native);
    for row in &rows {
        source
            .insert(row.clone(), RelationPersistence::Unlogged)
            .unwrap();
    }
    source
        .insert(row("private", None), RelationPersistence::Temporary)
        .unwrap();
    let captured = source.capture(&control).unwrap();
    source.replace(row("a", Some("replacement"))).unwrap();
    source.remove(&rows[1].relation).unwrap();
    drop(source);
    let expected = wire(&rows);
    assert_eq!(bytes(&captured), expected);
    let decoded = CatalogIndexes::read_image(
        &mut expected.as_slice(),
        &restored,
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(decoded.len(), 3);
    for row in &rows {
        assert!(decoded.is_durable(&row.relation));
        assert_eq!(
            wire(&[decoded.get(&row.relation).unwrap().clone()]),
            wire(std::slice::from_ref(row))
        );
    }
    let independent = decoded.capture(&control).unwrap();
    drop(decoded);
    assert_eq!(bytes(&independent), expected);
    drop((captured, independent));
    assert_eq!(native.used(), 0);
    assert_eq!(restored.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_index_images_reject_truncation_order_tags_and_invalid_identifiers() {
    let valid = wire(&[row("a", None), row("b", Some("opaque"))]);
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    for end in 0..valid.len() {
        assert!(CatalogIndexes::read_image(&mut &valid[..end], &memory, &cancellation).is_err());
        assert_eq!(memory.used(), 0);
    }
    let mut invalid_tag = wire(&[row("a", None)]);
    *invalid_tag.last_mut().unwrap() = 2;
    let mut invalid_utf8 = valid.clone();
    invalid_utf8[24] = 0xff;
    let mut invalid_rows = Vec::new();
    for field in 0..4 {
        for text in ["", "bad\0name"] {
            let mut value = row("a", None);
            *match field {
                0 => &mut value.relation.schema,
                1 => &mut value.relation.name,
                2 => &mut value.index_type,
                _ => &mut value.table_name,
            } = text.into();
            invalid_rows.push(wire(&[value]));
        }
    }
    invalid_rows.extend([
        invalid_tag,
        invalid_utf8,
        wire(&[row("a", None), row("a", None)]),
        wire(&[row("b", None), row("a", None)]),
        [valid.as_slice(), &[0]].concat(),
        [b"BADIMAGE".as_slice(), &valid[8..]].concat(),
    ]);
    for bytes in invalid_rows {
        assert!(CatalogIndexes::read_image(&mut bytes.as_slice(), &memory, &cancellation).is_err());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_index_image_admission_funds_keys_payloads_and_native_lifetimes() {
    let cancellation = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    let bytes = wire(&[row("a", Some("original"))]);
    let decoded =
        CatalogIndexes::read_image(&mut bytes.as_slice(), &memory, &cancellation).unwrap();
    let required = memory.used();
    drop(decoded);
    assert_eq!(memory.used(), 0);
    let exact = MemoryBudget::new(required);
    let decoded = CatalogIndexes::read_image(&mut bytes.as_slice(), &exact, &cancellation).unwrap();
    let mut clone = decoded.clone();
    drop(decoded);
    assert_eq!(exact.used(), required);
    clone.clear_durable();
    assert_eq!(exact.used(), 0);
    let denied = MemoryBudget::new(required - 1);
    assert_eq!(
        CatalogIndexes::read_image(&mut bytes.as_slice(), &denied, &cancellation)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(denied.used(), 0);
    assert!(CatalogIndexes::read_image(
        &mut wire(&[]).as_slice(),
        &MemoryBudget::new(0),
        &cancellation
    )
    .unwrap()
    .is_empty());
    let mut retained = Vec::new();
    for padding in [0, 1024] {
        let mut value = row("a", None);
        for field in [
            &mut value.relation.schema,
            &mut value.relation.name,
            &mut value.index_type,
            &mut value.table_name,
            &mut value.columns_json,
            &mut value.parameters_json,
        ] {
            field.push_str(&"x".repeat(padding));
        }
        let bytes = wire(&[value]);
        let mut decoded = None;
        let measured = allocation_counter::measure(|| {
            decoded = Some(
                CatalogIndexes::read_image(&mut bytes.as_slice(), &memory, &cancellation).unwrap(),
            );
        });
        retained.push((measured.bytes_current, memory.used()));
        drop(decoded);
        assert_eq!(memory.used(), 0);
    }
    assert_eq!(
        retained[1].0 - retained[0].0,
        i64::try_from(retained[1].1 - retained[0].1).unwrap()
    );
}

#[test]
fn native_index_image_output_has_constant_workspace_and_terminal_failure() {
    let mut source = CatalogIndexes::default();
    source
        .insert(
            row("a", Some(&"x".repeat(4 << 20))),
            RelationPersistence::Permanent,
        )
        .unwrap();
    let capture = StorageReadControl::with_limit(8 << 20);
    let captured = source.capture(&capture).unwrap();
    drop(source);
    let control = StorageReadControl::with_limit(4096);
    let mut output = vec![0; 32768];
    let measured = allocation_counter::measure(|| {
        let mut reader = captured.image_reader(&control).unwrap();
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        let mut total = 0;
        loop {
            let count = reader.read(&mut output).unwrap();
            assert!(count <= 16384);
            if count == 0 {
                break;
            }
            total += count;
        }
        assert!(total > 4 << 20);
    });
    assert!(measured.bytes_total < 4096, "{measured:?}");
    assert_eq!(control.memory().used(), 0);
    let mut reader = captured.image_reader(&control).unwrap();
    assert_eq!(reader.read(&mut [0]).unwrap(), 1);
    control.cancellation().cancel();
    assert!(reader.read(&mut [0]).is_err());
    control.cancellation().reset();
    assert_eq!(control.memory().used(), 0);
    assert!(reader.read(&mut [0]).is_err());
    drop(reader);
    drop(captured);
    assert_eq!(capture.memory().used(), 0);
}
