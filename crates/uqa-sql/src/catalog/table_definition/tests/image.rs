//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{PartitionBound, TableCheck, TableHierarchy},
    catalog::image::{self, DefinitionImageReader},
};
use std::io::Read;

trait Image: TableDefinitionValue + std::fmt::Debug {
    fn output<'a>(
        &'a self,
        memory: &'a MemoryBudget,
        token: &'a CancellationToken,
    ) -> Result<DefinitionImageReader<'a>, SQLError>;
    fn raw(
        input: &mut dyn Read,
        memory: &MemoryBudget,
        token: &CancellationToken,
    ) -> Result<(Budgeted<Self>, usize), SQLError>;
    fn native(
        input: &mut dyn Read,
        memory: &MemoryBudget,
        token: &CancellationToken,
    ) -> Result<TableDefinition<Self>, SQLError>;
}

macro_rules! image_type {
    ($ty:ty, $output:ident, $input:ident) => {
        impl Image for $ty {
            fn output<'a>(
                &'a self,
                memory: &'a MemoryBudget,
                token: &'a CancellationToken,
            ) -> Result<DefinitionImageReader<'a>, SQLError> {
                image::$output(self, memory, token)
            }
            fn raw(
                input: &mut dyn Read,
                memory: &MemoryBudget,
                token: &CancellationToken,
            ) -> Result<(Budgeted<Self>, usize), SQLError> {
                image::$input(input, memory, token)
            }
            fn native(
                input: &mut dyn Read,
                memory: &MemoryBudget,
                token: &CancellationToken,
            ) -> Result<TableDefinition<Self>, SQLError> {
                TableDefinition::<Self>::read_image(input, memory, token)
            }
        }
    };
}
image_type!(
    Vec<ColumnDef>,
    columns_image_reader,
    read_columns_image_with_footprint
);
image_type!(
    Vec<TableCheck>,
    table_checks_image_reader,
    read_table_checks_image_with_footprint
);
image_type!(
    TableHierarchy,
    table_hierarchy_image_reader,
    read_table_hierarchy_image_with_footprint
);

fn encoded<T: Image>(value: &T) -> Vec<u8> {
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let mut output = value.output(&memory, &token).unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 127];
    loop {
        let count = output.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    bytes
}

fn check(value: Expr) -> Vec<TableCheck> {
    vec![TableCheck {
        name: Some("original".into()),
        object_id: Some([7; 16]),
        catalog_oid: Some(4172),
        is_local: false,
        expr: value,
        enforced: true,
        validated: false,
        no_inherit: true,
        partition_constraint: None,
    }]
}

fn round_trip<T: Image>(original: T, retained_literal: bool) {
    let native = TableDefinition::new(&original).unwrap();
    let bytes = encoded(&*native);
    drop((original, native));
    let token = CancellationToken::new();
    let probe = MemoryBudget::new(8 << 20);
    let (raw, children) = T::raw(&mut bytes.as_slice(), &probe, &token).unwrap();
    assert_eq!(probe.used(), raw.reserved_bytes() + children);
    if retained_literal {
        assert!(children > 1 << 20);
    }
    let limit = probe.peak() + size_of::<Budgeted<T>>();
    drop(raw);
    assert_eq!(probe.used(), 0);

    // The native owner receives only the decoder's peak plus its fixed shared header.
    // A second population copy cannot fit this original allowance.
    let memory = MemoryBudget::new(limit);
    let restored = T::native(&mut bytes.as_slice(), &memory, &token).unwrap();
    let bytes_retained = restored.retained_bytes();
    assert_eq!(memory.used(), bytes_retained);
    assert_eq!(encoded(&*restored), bytes);
    let independent = MemoryBudget::new(bytes_retained);
    let control = ProductionControl::new(&independent, &token, &token);
    let mut captured = None;
    let allocations = allocation_counter::measure(|| {
        captured = Some(restored.capture_with_control(&control).unwrap());
    });
    assert_eq!(allocations.count_total, 0);
    let captured = captured.unwrap();
    assert!(restored.shares_definition(&captured));
    assert_eq!(independent.used(), bytes_retained);
    let denied = MemoryBudget::new(bytes_retained - 1);
    assert!(restored
        .capture_with_control(&ProductionControl::new(&denied, &token, &token))
        .is_err());
    assert_eq!(denied.used(), 0);
    let shared = restored.shared();
    drop((restored, captured));
    assert_eq!(independent.used(), 0);
    assert_eq!(memory.used(), bytes_retained);
    assert_eq!(encoded(shared.as_ref()), bytes);
    drop(shared);
    assert_eq!(memory.used(), 0);
}

#[test]
fn restored_table_definition_images_transfer_native_ownership_without_reconstruction() {
    let crate::Statement::CreateTable(table) =
        crate::compile("CREATE TABLE t(v integer DEFAULT 42, CONSTRAINT positive CHECK(v > 0))")
            .unwrap()
            .remove(0)
    else {
        panic!("table")
    };
    round_trip(table.columns, false);
    round_trip(table.checks, false);
    round_trip(table.hierarchy, false);
}

#[test]
fn restored_table_definition_images_admit_every_retained_literal_owner() {
    let reads = Arc::new(AtomicUsize::new(0));
    let column = column(Arc::new(Source(reads)), 0);
    let literal = column.default.clone().unwrap();
    round_trip(vec![column], true);
    round_trip(check(literal.clone()), true);
    round_trip(
        TableHierarchy {
            parents: vec!["public.parent".into()],
            parent_sequence_numbers: vec![2],
            partition_bound: Some(PartitionBound::List(vec![literal])),
            ..TableHierarchy::default()
        },
        true,
    );
}

fn failed_input<T: Image>(value: T) {
    let bytes = encoded(&value);
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    for end in [0, 7, 15, bytes.len() / 2, bytes.len() - 1] {
        assert!(T::native(&mut &bytes[..end], &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(T::native(&mut trailing.as_slice(), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
    drop(T::native(&mut bytes.as_slice(), &memory, &token).unwrap());
    for limit in [0, 1, memory.peak() - 1] {
        let denied = MemoryBudget::new(limit);
        assert_eq!(
            T::native(&mut bytes.as_slice(), &denied, &token)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(denied.used(), 0);
    }
    let final_token = CancellationToken::new();
    let mut input = CancelAtEnd {
        bytes: &bytes,
        token: &final_token,
    };
    assert!(T::native(&mut input, &memory, &final_token).is_err());
    assert_eq!(memory.used(), 0);
    token.cancel();
    assert!(T::native(&mut bytes.as_slice(), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

struct CancelAtEnd<'a> {
    bytes: &'a [u8],
    token: &'a CancellationToken,
}

impl Read for CancelAtEnd<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if self.bytes.is_empty() {
            self.token.cancel();
        }
        self.bytes.read(output)
    }
}

#[test]
fn restored_table_definition_images_release_failed_input_and_native_admission() {
    let crate::Statement::CreateTable(table) =
        crate::compile("CREATE TABLE t(v integer DEFAULT 42, CONSTRAINT positive CHECK(v > 0))")
            .unwrap()
            .remove(0)
    else {
        panic!("table")
    };
    failed_input(table.columns);
    failed_input(table.checks);
    failed_input(table.hierarchy);
}
