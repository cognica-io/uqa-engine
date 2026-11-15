//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{Expr, PartitionBound, PartitionRangeDatum, Statement},
    binding::fixture::{empty_binding_context, NoRoutines},
    catalog::{
        type_definitions::TypeDefinitions,
        type_references::{TypeReferenceInput, TypeReferences},
    },
    routines::native::NativeRoutineReferences,
};
use uqa_core::Value;

struct Table {
    name: RelationIdentity,
    columns: Vec<ColumnDef>,
    hierarchy: TableHierarchy,
    checks: Vec<TableCheck>,
}

fn tables(declaration: &str, bounds: Vec<PartitionBound>) -> Vec<Table> {
    let Statement::CreateTable(parent) = crate::compile(declaration).unwrap().remove(0) else {
        panic!("table declaration");
    };
    let mut result = vec![Table {
        name: RelationIdentity::new("public", "parent"),
        columns: parent.columns,
        hierarchy: parent.hierarchy,
        checks: parent.checks,
    }];
    for (index, bound) in bounds.into_iter().enumerate() {
        result.push(Table {
            name: RelationIdentity::new("public", format!("child{index}")),
            columns: result[0].columns.clone(),
            hierarchy: TableHierarchy {
                parents: vec!["public.parent".into()],
                partition_bound: Some(bound),
                ..TableHierarchy::default()
            },
            checks: Vec::new(),
        });
    }
    result
}

fn snapshot(tables: &[Table]) -> serde_json::Value {
    serde_json::to_value(
        tables
            .iter()
            .map(|table| (&table.columns, &table.hierarchy, &table.checks))
            .collect::<Vec<_>>(),
    )
    .unwrap()
}

fn check(
    tables: &[Table],
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> Result<(), SQLError> {
    let original = snapshot(tables);
    let control = ProductionControl::new(memory, token, token);
    let binding = empty_binding_context().copy_with_control(&control)?;
    let references = NativeRoutineReferences::new(std::iter::empty(), memory, token)?;
    let input = TypeReferenceInput {
        domains: &TypeDefinitions::default(),
        enums: &TypeDefinitions::default(),
        composites: &TypeDefinitions::default(),
        ranges: &TypeDefinitions::default(),
        shells: &TypeDefinitions::default(),
    };
    let types = TypeReferences::new(input, std::iter::empty(), memory, token)?;
    let scalar = NativeScalarContext {
        subject: "partition",
        routines: &NoRoutines,
        references: &references,
        types: &types,
        binding: &binding,
    };
    let context = NativePartitionContext {
        scalar,
        values: None,
    };
    let result = validate(
        tables.iter().map(|table| NativePartitionTable {
            name: &table.name,
            columns: &table.columns,
            hierarchy: &table.hierarchy,
            checks: &table.checks,
        }),
        &context,
        memory,
        token,
    );
    assert_eq!(snapshot(tables), original);
    result
}

fn accepted(tables: &[Table]) {
    let memory = MemoryBudget::new(8 << 20);
    check(tables, &memory, &CancellationToken::new()).unwrap();
    assert_eq!(memory.used(), 0);
}

fn rejected(tables: &[Table]) {
    let memory = MemoryBudget::new(8 << 20);
    assert!(check(tables, &memory, &CancellationToken::new()).is_err());
    assert_eq!(memory.used(), 0);
}

fn list(values: Vec<Value>) -> PartitionBound {
    PartitionBound::List(values.into_iter().map(Expr::Literal).collect())
}

fn point(value: i64) -> PartitionRangeDatum {
    PartitionRangeDatum::Value(Expr::Literal(Value::Int(value)))
}

fn range(lower: i64, upper: i64) -> PartitionBound {
    PartitionBound::Range {
        lower: vec![point(lower)],
        upper: vec![point(upper)],
    }
}

#[test]
fn native_partition_bindings_preserve_stored_types_and_unevaluated_keys() {
    for ty in ["smallint", "integer", "bigint"] {
        accepted(&tables(
            &format!("CREATE TABLE parent(id {ty}) PARTITION BY LIST(id)"),
            vec![
                list(vec![Value::Int(1), Value::Null]),
                PartitionBound::Default,
            ],
        ));
    }
    accepted(&tables(
        "CREATE TABLE parent(id integer) PARTITION BY LIST((id / 0))",
        vec![list(vec![Value::Int(1)])],
    ));
    accepted(&tables(
        "CREATE TABLE parent(id boolean) PARTITION BY LIST(id)",
        vec![list(vec![Value::Bool(true)])],
    ));
    accepted(&tables(
        "CREATE TABLE parent(id text) PARTITION BY LIST(id)",
        vec![list(vec![Value::Str("x".into())])],
    ));
    accepted(&tables(
        "CREATE TABLE parent(id integer) PARTITION BY RANGE(id)",
        vec![range(1, 2), range(2, 3)],
    ));
    accepted(&tables(
        "CREATE TABLE parent(id integer) PARTITION BY HASH(id)",
        vec![
            PartitionBound::Hash {
                modulus: 4,
                remainder: 0,
            },
            PartitionBound::Hash {
                modulus: 8,
                remainder: 2,
            },
        ],
    ));
}

#[test]
fn native_partition_bindings_reject_untyped_and_mismatched_constants() {
    for bound in [
        list(vec![Value::Bool(true)]),
        list(vec![Value::Str("1".into())]),
        list(vec![Value::Int(i64::MAX)]),
        list(vec![]),
        list(vec![Value::Int(1), Value::Int(1)]),
        PartitionBound::List(vec![Expr::Column("id".into())]),
        PartitionBound::List(vec![Expr::TypedLiteral {
            value: Value::Int(1),
            ty: "bigint".into(),
            collation_oid: None,
            composite_source: None,
        }]),
        range(1, 2),
        PartitionBound::Hash {
            modulus: 2,
            remainder: 0,
        },
    ] {
        rejected(&tables(
            "CREATE TABLE parent(id integer) PARTITION BY LIST(id)",
            vec![bound],
        ));
    }
    rejected(&tables(
        "CREATE TABLE parent(id smallint) PARTITION BY LIST(id)",
        vec![list(vec![Value::Int(32768)])],
    ));
}

#[test]
fn native_partition_bindings_reject_invalid_key_scopes() {
    for key in [
        "1",
        "missing",
        "other.id",
        "$1",
        "sum(id)",
        "unnest(ARRAY[id])",
        "(SELECT id)",
    ] {
        rejected(&tables(
            &format!("CREATE TABLE parent(id integer) PARTITION BY LIST(({key}))"),
            vec![list(vec![Value::Int(1)])],
        ));
    }
    let mut input = tables(
        "CREATE TABLE parent(id integer) PARTITION BY LIST(id)",
        vec![list(vec![Value::Int(1)])],
    );
    input[0]
        .hierarchy
        .partition_spec
        .as_mut()
        .unwrap()
        .keys
        .clear();
    rejected(&input);
}

#[test]
fn native_partition_constant_carriers_keep_array_rank_and_original_domains() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let leaf = || {
        Value::Array(
            uqa_core::ArrayValue::with_lower_bounds(vec![Value::Int(1), Value::Null], vec![3])
                .unwrap(),
        )
    };
    let matrix = Value::Array(
        uqa_core::ArrayValue::with_lower_bounds(vec![leaf(), leaf()], vec![0, 3])
            .unwrap()
            .with_element_type_oid(Some(23)),
    );
    let ty = ColumnType::Array(Box::new(ColumnType::Integer));
    super::values::validate(&matrix, &ty, None, &control).unwrap();
    let domain = ColumnType::Domain {
        schema: "public".into(),
        name: "array_domain".into(),
        oid: 50000,
        array_oid: Some(50001),
        collation_oid: None,
        base: Box::new(ty.clone()),
    };
    let atomic = Value::Array(
        uqa_core::ArrayValue::with_lower_bounds(vec![leaf()], vec![1])
            .unwrap()
            .with_element_type_oid(Some(50000)),
    );
    super::values::validate(
        &atomic,
        &ColumnType::Array(Box::new(domain)),
        None,
        &control,
    )
    .unwrap();
    assert!(super::values::validate(&atomic, &ty, None, &control).is_err());
    let wrong_rank =
        Value::Array(uqa_core::ArrayValue::with_lower_bounds(vec![leaf()], vec![1]).unwrap());
    assert!(super::values::validate(&wrong_rank, &ty, None, &control).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_partition_bindings_reject_range_shape_and_overlaps() {
    for bounds in [
        vec![range(2, 1)],
        vec![range(1, 1)],
        vec![range(1, 3), range(2, 4)],
        vec![PartitionBound::Range {
            lower: vec![],
            upper: vec![point(2)],
        }],
        vec![PartitionBound::Range {
            lower: vec![PartitionRangeDatum::Value(Expr::Literal(Value::Null))],
            upper: vec![point(2)],
        }],
        vec![PartitionBound::Default, PartitionBound::Default],
    ] {
        rejected(&tables(
            "CREATE TABLE parent(id integer) PARTITION BY RANGE(id)",
            bounds,
        ));
    }
    let bound = PartitionBound::Range {
        lower: vec![PartitionRangeDatum::MinValue, point(1)],
        upper: vec![PartitionRangeDatum::MaxValue, PartitionRangeDatum::MaxValue],
    };
    rejected(&tables(
        "CREATE TABLE parent(id integer, other integer) PARTITION BY RANGE(id, other)",
        vec![bound],
    ));
    rejected(&tables(
        "CREATE TABLE parent(id integer) PARTITION BY LIST(id)",
        vec![list(vec![Value::Null]), list(vec![Value::Null])],
    ));
}

#[test]
fn native_partition_bindings_check_hash_chains_without_modulus_sized_work() {
    let hash = |modulus, remainder| PartitionBound::Hash { modulus, remainder };
    let declaration = "CREATE TABLE parent(id integer) PARTITION BY HASH(id)";
    for bounds in [
        vec![hash(0, 0)],
        vec![hash(2, -1)],
        vec![hash(2, 2)],
        vec![PartitionBound::Default],
        vec![hash(4, 0), hash(6, 1)],
        vec![hash(4, 0), hash(8, 4)],
    ] {
        rejected(&tables(declaration, bounds));
    }
    accepted(&tables(
        declaration,
        vec![hash(i32::MAX, 0), hash(i32::MAX, 1)],
    ));
    accepted(&tables(declaration, vec![hash(1 << 30, 0), hash(2, 1)]));
}

#[test]
fn native_partition_bindings_release_denied_and_cancelled_scratch() {
    let input = tables(
        "CREATE TABLE parent(id integer) PARTITION BY LIST(id)",
        vec![list(vec![Value::Int(1)])],
    );
    let mut outcomes = (0, 0);
    for limit in [0, 32, 512, 4096, 8 << 20] {
        let memory = MemoryBudget::new(limit);
        match check(&input, &memory, &CancellationToken::new()) {
            Ok(()) => outcomes.0 += 1,
            Err(error) => {
                assert_eq!(error.sqlstate(), Some("53200"), "{error}");
                outcomes.1 += 1;
            }
        }
        assert_eq!(memory.used(), 0);
    }
    assert!(outcomes.0 > 0 && outcomes.1 > 0);
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    token.cancel();
    assert!(check(&input, &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}
