//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{catalog::table_definition::TableDefinition, ColumnType};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

const WINDOW: ImageDecodeWindow = ImageDecodeWindow { bytes: 1, steps: 1 };

fn key(ty: ColumnType) -> ColumnDef {
    let mut column = ColumnDef::nullable("original_key", ty);
    column.primary_key = true;
    column
}

fn decoder(
    columns: TableDefinition<Vec<ColumnDef>>,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<Option<usize>, SQLError> {
    let original = original.clone();
    ImageDecoder::new(memory, invoking, move |mut input| async move {
        single_integer_primary_key_from(&columns, &original, &mut input.input())
            .await
            .map(|key| key.map(|name| name.as_ptr() as usize))
    })
    .unwrap()
}

#[test]
fn integer_primary_key_input_preserves_original_column_names_and_declared_types() {
    let mut cases = vec![vec![], vec![ColumnDef::nullable("body", ColumnType::Text)]];
    for ty in [
        ColumnType::SmallInteger,
        ColumnType::Integer,
        ColumnType::BigInteger,
        ColumnType::Oid,
        ColumnType::Xid,
        ColumnType::Text,
        ColumnType::Boolean,
    ] {
        cases.push(vec![key(ty.clone())]);
        cases.push(vec![
            ColumnDef::nullable("body", ColumnType::Text),
            key(ty.clone()),
        ]);
        cases.push(vec![key(ty.clone()), key(ColumnType::Integer)]);
        cases.push(vec![key(ColumnType::Domain {
            schema: "public".into(),
            name: "domain".into(),
            oid: 12345,
            array_oid: None,
            collation_oid: None,
            base: Box::new(ty),
        })]);
    }
    for columns in cases {
        let native = TableDefinition::new(&columns).unwrap();
        let mut keys = native.iter().filter(|column| column.primary_key);
        let expected = keys.next().and_then(|column| {
            (keys.next().is_none() && column.ty.is_integer())
                .then_some(column.name.as_ptr() as usize)
        });
        assert_eq!(
            single_integer_primary_key(&native).map(|name| name.as_ptr() as usize),
            expected
        );
        let memory = MemoryBudget::new(65536);
        let output = decoder(
            native,
            &memory,
            &CancellationToken::new(),
            &CancellationToken::new(),
        )
        .read(&mut std::io::empty())
        .unwrap();
        assert_eq!(output, expected);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn integer_primary_key_input_bounds_column_and_domain_work_without_copying() {
    let mut ty = ColumnType::Integer;
    for number in 0..32 {
        ty = ColumnType::Domain {
            schema: "public".into(),
            name: format!("d{number}"),
            oid: 12345 + number,
            array_oid: None,
            collation_oid: None,
            base: Box::new(ty),
        };
    }
    let mut columns = vec![ColumnDef::nullable("body", ColumnType::Text); 128];
    columns.push(key(ty));
    let columns = TableDefinition::new(&columns).unwrap();
    let expected = Some(columns.last().unwrap().name.as_ptr() as usize);
    let memory = MemoryBudget::new(65536);
    let mut input = decoder(
        columns.clone(),
        &memory,
        &CancellationToken::new(),
        &CancellationToken::new(),
    );
    let mut pending = 0;
    loop {
        let mut output = None;
        let allocation = allocation_counter::measure(|| {
            output = Some(input.advance(&mut std::io::empty(), WINDOW).unwrap());
        });
        assert_eq!(allocation.count_total, 0);
        if let Some(output) = output.unwrap() {
            assert_eq!(output, expected);
            break;
        }
        pending += 1;
    }
    assert!(pending >= columns.len() + 32);
    assert_eq!(memory.used(), 0);
    for stop in 0..pending {
        for cancel_original in [false, true] {
            let memory = MemoryBudget::new(65536);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = decoder(columns.clone(), &memory, &original, &invoking);
            for _ in 0..stop {
                assert!(input
                    .advance(&mut std::io::empty(), WINDOW)
                    .unwrap()
                    .is_none());
            }
            if cancel_original {
                original.cancel();
            } else {
                invoking.cancel();
            }
            assert!(matches!(
                input.advance(&mut std::io::empty(), WINDOW),
                Err(SQLError::Cancelled(_))
            ));
            assert!(input.advance(&mut std::io::empty(), WINDOW).is_err());
            assert_eq!(memory.used(), 0);
        }
    }
    assert_eq!(
        single_integer_primary_key(&columns).map(|name| name.as_ptr() as usize),
        expected
    );
}
