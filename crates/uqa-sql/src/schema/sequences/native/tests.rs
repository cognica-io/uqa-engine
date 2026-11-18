//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::AutoIncrement, ColumnType};
use std::sync::Arc;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

#[test]
fn native_sequence_function_relations_preserve_numeric_and_quoted_names() {
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for reference in ["-", "0", "010", "2147483647", "4294967295"] {
        assert!(function_relation(reference, &control).unwrap().is_none());
    }
    let named = function_relation("\"saved schema\".\"123\"", &control)
        .unwrap()
        .unwrap();
    assert_eq!(*named, RelationIdentity::new("saved schema", "123"));
    drop(named);
    for reference in ["unqualified", "+5", " 5", "08", "4294967296"] {
        assert!(
            function_relation(reference, &control).is_err(),
            "{reference}"
        );
    }
    assert_eq!(memory.used(), 0);
    let empty = MemoryBudget::new(0);
    assert_eq!(
        function_relation(
            "public.sequence",
            &ProductionControl::new(&empty, &token, &token)
        )
        .unwrap_err()
        .sqlstate(),
        Some("53200")
    );
    assert_eq!(empty.used(), 0);
    token.cancel();
    assert_eq!(
        function_relation("2147483647", &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
}

fn column() -> ColumnDef {
    let mut column = ColumnDef::nullable("id", ColumnType::Integer);
    column.object_id = Some([2; 16]);
    column
}

fn owner(dependency: SequenceOwnerDependency) -> SequenceOwner {
    SequenceOwner {
        table_object_id: [1; 16],
        column_object_id: [2; 16],
        dependency,
    }
}

fn check(
    tables: &[([u8; 16], Vec<ColumnDef>)],
    sequences: &[(RelationIdentity, Option<SequenceOwner>)],
) -> Result<(), String> {
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let result = validate(
        tables.iter().map(|(id, columns)| (*id, columns.as_slice())),
        sequences.iter().map(|(name, owner)| (name, *owner)),
        &memory,
        &token,
    )
    .map_err(|error| error.to_string());
    assert_eq!(memory.used(), 0);
    result
}

#[test]
fn native_sequence_owners_use_original_column_incarnations_without_rebinding() {
    let tables = vec![([1; 16], vec![column()])];
    let name = RelationIdentity::new("public", "s");
    let mut owned = owner(SequenceOwnerDependency::Automatic);
    check(&tables, &[(name.clone(), Some(owned))]).unwrap();
    owned.table_object_id = [3; 16];
    assert!(check(&tables, &[(name.clone(), Some(owned))]).is_err());
    owned.table_object_id = [1; 16];
    owned.column_object_id = [3; 16];
    assert!(check(&tables, &[(name, Some(owned))]).is_err());
}

#[test]
fn native_sequence_columns_preserve_inherited_provenance_and_legacy_counters() {
    let mut serial = column();
    serial.auto_increment = Some(AutoIncrement {
        sequence: Some("public.s".into()),
        ..AutoIncrement::serial()
    });
    let mut legacy = column();
    legacy.object_id = Some([3; 16]);
    legacy.auto_increment = Some(AutoIncrement::legacy());
    let tables = vec![
        ([1; 16], vec![serial.clone(), legacy]),
        ([4; 16], vec![serial]),
    ];
    let sequences = [(
        RelationIdentity::new("public", "s"),
        Some(owner(SequenceOwnerDependency::Automatic)),
    )];
    let before = serde_json::to_string(&tables).unwrap();
    check(&tables, &sequences).unwrap();
    assert_eq!(serde_json::to_string(&tables).unwrap(), before);
    assert!(check(&tables, &[]).is_err());
}

#[test]
fn native_sequence_internal_owners_require_the_original_identity_column_and_sequence() {
    let name = RelationIdentity::new("public", "s");
    let sequences = [(name.clone(), Some(owner(SequenceOwnerDependency::Internal)))];
    let mut tables = vec![([1; 16], vec![column()])];
    assert!(check(&tables, &sequences).is_err());
    tables[0].1[0].auto_increment = Some(AutoIncrement {
        sequence: Some("public.s".into()),
        ..AutoIncrement::identity_always()
    });
    check(&tables, &sequences).unwrap();
    let mut wrong = sequences.to_vec();
    wrong[0].0.name = "other".into();
    assert!(check(&tables, &wrong).is_err());
    tables[0].1[0].auto_increment.as_mut().unwrap().kind = crate::ast::AutoIncrementKind::Serial;
    assert!(check(&tables, &sequences).is_err());
}

#[test]
fn native_sequence_references_reject_unbound_names_and_unconsumed_declarations() {
    let name = RelationIdentity::new("public", "s");
    let mut tables = vec![([1; 16], vec![column()])];
    for reference in [None, Some("s".into()), Some("public.missing".into())] {
        tables[0].1[0].auto_increment = Some(AutoIncrement {
            sequence: reference,
            ..AutoIncrement::serial()
        });
        assert!(check(&tables, &[(name.clone(), None)]).is_err());
    }
    tables[0].1[0].auto_increment = Some(AutoIncrement {
        sequence: Some("public.s".into()),
        declaration: Some(Box::default()),
        ..AutoIncrement::identity_by_default()
    });
    assert!(check(&tables, &[(name, None)]).is_err());
}

#[test]
fn native_sequence_validation_rejects_duplicate_owners_and_releases_denied_workspace() {
    let tables = [([1; 16], vec![column()])];
    assert!(check(&[tables[0].clone(), tables[0].clone()], &[]).is_err());
    assert!(check(&[([1; 16], vec![column(), column()])], &[]).is_err());
    let token = CancellationToken::new();
    for limit in [0, 1] {
        let memory = MemoryBudget::new(limit);
        assert!(validate(
            tables.iter().map(|(id, c)| (*id, c.as_slice())),
            std::iter::empty(),
            &memory,
            &token
        )
        .is_err());
        assert_eq!(memory.used(), 0);
    }
    token.cancel();
    let memory = MemoryBudget::new(64 << 10);
    assert!(validate(
        tables.iter().map(|(id, c)| (*id, c.as_slice())),
        std::iter::empty(),
        &memory,
        &token
    )
    .is_err());
    assert_eq!(memory.used(), 0);
}

struct Catalog {
    tables: Vec<([u8; 16], Vec<ColumnDef>)>,
    sequences: Vec<(RelationIdentity, Option<SequenceOwner>)>,
}

impl Catalog {
    fn read(
        self: &Arc<Self>,
        memory: &MemoryBudget,
        original: &CancellationToken,
    ) -> ImageDecoder<(), SQLError> {
        let catalog = self.clone();
        let original = original.clone();
        ImageDecoder::new(memory, &CancellationToken::new(), |mut reader| async move {
            let mut work = reader.input();
            validate_with_work(
                catalog.tables.iter().map(table_input),
                catalog.sequences.iter().map(sequence_input),
                work.memory,
                &original,
                &mut work,
            )
            .await
        })
        .unwrap()
    }

    fn ordinary(&self) -> Result<(), String> {
        let memory = MemoryBudget::new(4 << 20);
        let result = validate(
            self.tables.iter().map(table_input),
            self.sequences.iter().map(sequence_input),
            &memory,
            &CancellationToken::new(),
        )
        .map_err(|error| error.to_string());
        assert_eq!(memory.used(), 0);
        result
    }
}

fn table_input(row: &([u8; 16], Vec<ColumnDef>)) -> ([u8; 16], &[ColumnDef]) {
    (row.0, &row.1)
}

fn sequence_input(
    row: &(RelationIdentity, Option<SequenceOwner>),
) -> (&RelationIdentity, Option<SequenceOwner>) {
    (&row.0, row.1)
}

fn advance(reader: &mut ImageDecoder<(), SQLError>, steps: usize) -> Result<Option<()>, SQLError> {
    reader.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
}

fn drain(catalog: &Arc<Catalog>, steps: usize) -> (usize, usize, Result<(), String>) {
    let memory = MemoryBudget::new(4 << 20);
    let mut reader = catalog.read(&memory, &CancellationToken::new());
    let mut decreasing = 0;
    let largest_node = OwnedMap::<([u8; 16], [u8; 16]), &ColumnDef>::entry_bytes()
        .max(OwnedMap::<[u8; 16], &[ColumnDef]>::entry_bytes())
        .max(OwnedMap::<&RelationIdentity, ()>::entry_bytes());
    for polls in 1..1_000_000 {
        let before = memory.used();
        let result = advance(&mut reader, steps);
        let used = memory.used();
        if steps == 1 {
            assert!(
                used.saturating_sub(before) <= largest_node,
                "one work unit admitted multiple sequence index nodes"
            );
        }
        if used < before {
            decreasing += 1;
        }
        match result {
            Ok(None) => (),
            final_result => {
                assert_eq!(memory.used(), 0);
                return (
                    polls,
                    decreasing,
                    final_result
                        .map(|done| done.unwrap())
                        .map_err(|error| error.to_string()),
                );
            }
        }
    }
    panic!("sequence validation did not finish");
}

#[test]
fn native_sequence_validation_yields_for_original_tables_columns_owners_and_scratch_retirement() {
    const WIDTH: usize = 4096;
    let columns: Vec<_> = (0..WIDTH)
        .map(|index| {
            let mut item = column();
            item.object_id = Some((index as u128).to_be_bytes());
            item
        })
        .collect();
    let mut tables: Vec<_> = (0..WIDTH)
        .map(|index| ((index as u128).to_be_bytes(), Vec::new()))
        .collect();
    tables[0].1 = columns;
    let sequences = (0..WIDTH)
        .map(|index| {
            (
                RelationIdentity::new("public", format!("s{index:04}")),
                Some(SequenceOwner {
                    table_object_id: [0; 16],
                    column_object_id: (index as u128).to_be_bytes(),
                    dependency: SequenceOwnerDependency::Automatic,
                }),
            )
        })
        .collect();
    let mut catalog = Arc::new(Catalog { tables, sequences });
    assert_eq!(catalog.ordinary(), Ok(()));
    let (one, decreasing, result) = drain(&catalog, 1);
    result.unwrap();
    assert!(one > WIDTH * 5);
    assert!(decreasing >= WIDTH * 3, "{decreasing}");
    let (batch, _, result) = drain(&catalog, 32);
    result.unwrap();
    assert_eq!(batch, one.div_ceil(32));
    for cancel in [false, true] {
        let original = CancellationToken::new();
        let memory = MemoryBudget::new(4 << 20);
        let mut reader = catalog.read(&memory, &original);
        for _ in 0..WIDTH {
            assert!(advance(&mut reader, 1).unwrap().is_none());
        }
        if cancel {
            original.cancel();
            assert_eq!(
                advance(&mut reader, 1).unwrap_err().sqlstate(),
                Some("57014")
            );
        }
        drop(reader);
        assert_eq!(memory.used(), 0);
    }
    let limited = MemoryBudget::new(64 << 10);
    let denied = catalog.read(&limited, &CancellationToken::new());
    assert_eq!(
        denied.read(&mut std::io::empty()).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(limited.used(), 0);
    Arc::get_mut(&mut catalog)
        .unwrap()
        .sequences
        .last_mut()
        .unwrap()
        .1
        .as_mut()
        .unwrap()
        .column_object_id = [255; 16];
    let expected = catalog.ordinary().unwrap_err();
    assert!(expected.contains("missing owned column identity"));
    let (late, _, error) = drain(&catalog, 1);
    assert!(late > WIDTH * 3);
    assert_eq!(error.unwrap_err(), expected);
}

#[test]
fn native_sequence_name_comparisons_yield_within_shared_utf8_prefixes_and_reject_the_first_conflict(
) {
    let prefix = "記".repeat(65536);
    let name = RelationIdentity::new(prefix.clone(), format!("{prefix}a"));
    let catalog = Arc::new(Catalog {
        tables: Vec::new(),
        sequences: vec![
            (name.clone(), None),
            (name, Some(owner(SequenceOwnerDependency::Internal))),
        ],
    });
    let expected = catalog.ordinary().unwrap_err();
    assert!(expected.contains("duplicate sequence name"));
    let (single, _, error) = drain(&catalog, 1);
    assert!(single > (2 * prefix.len()) / 4096, "{single}");
    assert_eq!(error.unwrap_err(), expected);
    let (batch, _, error) = drain(&catalog, 32);
    assert_eq!(batch, single.div_ceil(32));
    assert_eq!(error.unwrap_err(), expected);
}

#[test]
fn implicit_sequence_checks_yield_across_empty_tables_and_columns_without_object_ids() {
    const WIDTH: usize = 4096;
    let plain = ColumnDef::nullable("plain", ColumnType::Integer);
    let mut implicit = plain.clone();
    implicit.auto_increment = Some(AutoIncrement::serial());
    let mut wide = vec![plain; WIDTH];
    wide.push(implicit.clone());
    let mut tables: Vec<_> = (0..WIDTH)
        .map(|index| ((index as u128).to_be_bytes(), Vec::new()))
        .collect();
    tables.push(([255; 16], vec![implicit]));
    for tables in [vec![([0; 16], wide)], tables] {
        let catalog = Arc::new(Catalog {
            tables,
            sequences: Vec::new(),
        });
        let expected = catalog.ordinary().unwrap_err();
        assert!(expected.contains("implicit column has no sequence"));
        let (single, _, result) = drain(&catalog, 1);
        assert!(single > 2 * WIDTH, "{single}");
        assert_eq!(result.unwrap_err(), expected);
    }
}
