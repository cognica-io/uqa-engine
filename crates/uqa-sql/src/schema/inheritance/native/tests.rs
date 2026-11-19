//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::PartitionSpec;

type Table = (RelationIdentity, TableHierarchy, RelationPersistence);

fn table(name: &str, parents: &[&str]) -> Table {
    (
        RelationIdentity::new("public", name),
        TableHierarchy {
            parents: parents.iter().map(|name| (*name).into()).collect(),
            ..TableHierarchy::default()
        },
        RelationPersistence::Permanent,
    )
}

fn check(tables: &[Table]) -> Result<(), String> {
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let result = validate(
        tables
            .iter()
            .map(|(name, hierarchy, persistence)| (name, hierarchy, *persistence)),
        &memory,
        &token,
    )
    .map_err(|error| error.to_string());
    assert_eq!(memory.used(), 0);
    result
}

#[test]
fn native_hierarchy_preserves_diamond_and_quoted_parent_identity() {
    let tables = [
        table("root", &[]),
        table("left", &["public.root"]),
        table("right", &["public.root"]),
        table("leaf", &["public.left", "public.right"]),
        table("quoted.\"root", &[]),
        table("quoted_child", &["public.\"quoted.\"\"root\""]),
    ];
    let before = serde_json::to_string(&tables).unwrap();
    check(&tables).unwrap();
    assert_eq!(serde_json::to_string(&tables).unwrap(), before);
}

#[test]
fn native_hierarchy_rejects_missing_repeated_unbound_and_cyclic_parents() {
    for tables in [
        vec![table("child", &["public.absent"])],
        vec![
            table("root", &[]),
            table("child", &["public.root", "public.root"]),
        ],
        vec![table("root", &[]), table("child", &["root"])],
        vec![table("self", &["public.self"])],
        vec![
            table("left", &["public.right"]),
            table("right", &["public.left"]),
        ],
        vec![table("root", &[]), table("root", &[])],
    ] {
        assert!(check(&tables).is_err());
    }
}

#[test]
fn native_hierarchy_retains_sparse_sequence_numbers_and_rejects_invalid_membership() {
    let mut tables = [
        table("a", &[]),
        table("b", &[]),
        table("c", &["public.a", "public.b"]),
    ];
    for numbers in [vec![], vec![2, 7], vec![7, 2]] {
        tables[2].1.parent_sequence_numbers = numbers;
        check(&tables).unwrap();
    }
    for numbers in [vec![1], vec![1, 1], vec![0, 2], vec![-1, 2], vec![1, 2, 3]] {
        tables[2].1.parent_sequence_numbers = numbers;
        assert!(check(&tables).is_err());
    }
}

#[test]
fn native_hierarchy_requires_matching_partition_parent_kind() {
    for (strategy, bound) in [
        (PartitionStrategy::List, PartitionBound::List(vec![])),
        (
            PartitionStrategy::Range,
            PartitionBound::Range {
                lower: vec![],
                upper: vec![],
            },
        ),
        (
            PartitionStrategy::Hash,
            PartitionBound::Hash {
                modulus: 2,
                remainder: 1,
            },
        ),
    ] {
        let mut tables = [table("parent", &[]), table("child", &["public.parent"])];
        tables[0].1.partition_spec = Some(PartitionSpec {
            strategy,
            keys: vec![],
        });
        tables[1].1.partition_bound = Some(bound.clone());
        check(&tables).unwrap();
        tables[1].1.partition_bound = Some(PartitionBound::Default);
        assert_eq!(check(&tables).is_ok(), strategy != PartitionStrategy::Hash);
        tables[1].1.partition_bound = None;
        assert!(check(&tables).is_err());
        tables[1].1.partition_bound = Some(bound);
        tables[0].1.partition_spec = None;
        assert!(check(&tables).is_err());
        tables[1].1.parents.clear();
        assert!(check(&tables).is_err());
    }
}

#[test]
fn native_hierarchy_rejects_partition_strategy_mismatch_and_invalid_temporary_edges() {
    let mut tables = [table("parent", &[]), table("child", &["public.parent"])];
    tables[0].1.partition_spec = Some(PartitionSpec {
        strategy: PartitionStrategy::List,
        keys: vec![],
    });
    tables[1].1.partition_bound = Some(PartitionBound::Hash {
        modulus: 1,
        remainder: 0,
    });
    assert!(check(&tables).is_err());
    tables[1].1.partition_bound = Some(PartitionBound::Default);
    tables[1].2 = RelationPersistence::Temporary;
    assert!(check(&tables).is_err());
    tables[0].2 = RelationPersistence::Temporary;
    check(&tables).unwrap();
    tables[0].1.partition_spec = None;
    tables[1].1.partition_bound = None;
    tables[1].2 = RelationPersistence::Permanent;
    assert!(check(&tables).is_err());
}

#[test]
fn native_hierarchy_deep_ancestry_is_iterative_and_budget_failure_retires_scratch() {
    let mut tables = vec![table("t0", &[])];
    for index in 1..10_000 {
        tables.push(table(
            &format!("t{index}"),
            &[&format!("public.t{}", index - 1)],
        ));
    }
    check(&tables).unwrap();
    let encoded = serde_json::to_string(&tables).unwrap();
    let token = CancellationToken::new();
    for limit in [0, 1024, 32 << 10] {
        let memory = MemoryBudget::new(limit);
        assert!(validate(tables.iter().map(|(n, h, p)| (n, h, *p)), &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
    token.cancel();
    let memory = MemoryBudget::new(16 << 20);
    assert!(validate(tables.iter().map(|(n, h, p)| (n, h, *p)), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
    assert_eq!(serde_json::to_string(&tables).unwrap(), encoded);
}

struct Run {
    calls: usize,
    shrinking: usize,
    error: Option<(Option<String>, String)>,
}

fn drive(tables: Vec<Table>, steps: usize, limit: usize, cancel_at: Option<usize>) -> Run {
    use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};
    let memory = MemoryBudget::new(limit);
    let cancellation = CancellationToken::new();
    let original = cancellation.clone();
    let mut decoder = ImageDecoder::new(&memory, &cancellation, move |mut reader| async move {
        let mut input = reader.input();
        let declarations = tables
            .iter()
            .map(|(name, hierarchy, persistence)| (name, hierarchy, *persistence));
        validate_with_work(declarations, input.memory, &original, &mut input).await
    })
    .unwrap();
    let mut calls = 0;
    let mut shrinking = 0;
    let mut longest = 0;
    let error = loop {
        if cancel_at == Some(calls) {
            cancellation.cancel();
        }
        let before = memory.used();
        calls += 1;
        let result = decoder.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps });
        if memory.used() < before {
            shrinking += 1;
            longest = longest.max(shrinking);
        } else {
            shrinking = 0;
        }
        match result {
            Ok(None) => (),
            Ok(Some(())) => break None,
            Err(error) => break Some((error.sqlstate().map(str::to_owned), error.to_string())),
        }
    };
    drop(decoder);
    assert_eq!(memory.used(), 0);
    Run {
        calls,
        shrinking: longest,
        error,
    }
}

#[test]
fn native_hierarchy_yields_for_parent_sequences_edges_cycles_and_retirement() {
    const WIDTH: usize = 4096;
    const LIMIT: usize = 16 << 20;
    let mut child = table("zz_child", &[]);
    let mut tables = Vec::with_capacity(WIDTH + 1);
    for index in 0..WIDTH {
        let name = format!("parent_{index:04}");
        tables.push(table(&name, &[]));
        child.1.parents.push(format!("public.{name}"));
        child
            .1
            .parent_sequence_numbers
            .push(i32::try_from(index + 1).unwrap());
    }
    tables.push(child);
    let mut bad_sequence = tables.clone();
    *bad_sequence[WIDTH]
        .1
        .parent_sequence_numbers
        .last_mut()
        .unwrap() = 0;
    bad_sequence[WIDTH].1.parents[0] = "public.missing".to_owned();
    let bad = drive(bad_sequence.clone(), 1, LIMIT, None);
    let grouped = drive(bad_sequence, 32, LIMIT, None);
    assert!(bad.calls > WIDTH);
    assert!(grouped.calls * 20 < bad.calls);
    assert_eq!(bad.error, grouped.error);
    assert!(bad.error.unwrap().1.contains("parent sequence identity"));

    let mut missing = tables.clone();
    *missing[WIDTH].1.parents.last_mut().unwrap() = "public.missing".to_owned();
    let missing = drive(missing, 1, LIMIT, None);
    assert!(missing.calls > bad.calls + WIDTH * 2);
    assert!(missing.error.unwrap().1.contains("missing parent table"));

    let mut cyclic = tables.clone();
    cyclic[0].1.parents.push("public.zz_child".to_owned());
    let cyclic = drive(cyclic, 1, LIMIT, None);
    assert!(cyclic.calls > missing.calls + WIDTH);
    assert!(cyclic.error.unwrap().1.contains("cyclic inheritance"));

    let valid = drive(tables.clone(), 1, LIMIT, None);
    assert!(valid.error.is_none());
    assert!(valid.shrinking >= WIDTH);
    let cancelled = drive(tables.clone(), 1, LIMIT, Some(valid.calls - WIDTH / 2));
    assert_eq!(cancelled.error.unwrap().0.as_deref(), Some("57014"));
    let denied = drive(tables, 32, 32 << 10, None);
    assert_eq!(denied.error.unwrap().0.as_deref(), Some("53200"));
}

#[test]
fn native_hierarchy_yields_while_comparing_quoted_parent_names_and_reporting_persistence() {
    const LIMIT: usize = 4 << 20;
    let run = |length, steps, cancel_at| {
        let mut parent = table(&format!("x{}\"", "é".repeat(length)), &[]);
        let spelling = parent.0.qualified_name();
        let child = table("zz_child", &[&spelling]);
        parent.2 = RelationPersistence::Temporary;
        let ordinary = super::super::check_parent_persistence(&spelling, parent.2, child.2, false)
            .unwrap_err();
        let result = drive(vec![parent, child], steps, LIMIT, cancel_at);
        if cancel_at.is_none() {
            assert_eq!(
                result.error.as_ref().unwrap(),
                &(Some("42809".to_owned()), ordinary.to_string())
            );
        }
        result
    };
    let short = run(1, 1, None);
    let long = run(65_536, 1, None);
    let grouped = run(65_536, 32, None);
    assert!(long.calls > short.calls + 64);
    assert!(grouped.calls * 20 < long.calls);
    assert_eq!(grouped.error, long.error);
    let cancelled = run(65_536, 1, Some(long.calls - 2));
    assert_eq!(cancelled.error.unwrap().0.as_deref(), Some("57014"));
}
