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

fn check(tables: &[Table]) -> Result<(), SQLError> {
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let result = validate(
        tables
            .iter()
            .map(|(name, hierarchy, persistence)| (name, hierarchy, *persistence)),
        &memory,
        &token,
    );
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
