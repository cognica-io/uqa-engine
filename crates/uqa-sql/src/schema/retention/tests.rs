//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{
    AutoIncrement, AutoIncrementKind, AutoIncrementOwner, FunctionResolutionError, GeneratedColumn,
    GeneratedColumnKind, RoutineInvocationBinding, RoutineVariadicMode, Statement,
};

mod validation;

fn columns(sql: &str) -> Vec<ColumnDef> {
    let Statement::CreateTable(table) = crate::compile(sql).unwrap().remove(0) else {
        panic!("expected a table declaration");
    };
    table.columns
}

fn spare_text(text: &str, capacity: usize) -> String {
    let mut value = String::with_capacity(capacity);
    value.push_str(text);
    value
}

#[test]
fn retained_generations_preserve_identity_and_share_one_lease_at_full_allowance() {
    let mut source = Vec::with_capacity(7);
    source.extend(columns("CREATE TABLE t(v text DEFAULT 'original')"));
    source[0].name = spare_text("v", 4096);
    let source = Arc::new(source);
    let budget = MemoryBudget::new(1024 * 1024);
    let retained = RetainedColumns::capture(&source, &budget, &CancellationToken::new()).unwrap();
    assert_eq!(retained.as_ptr(), source.as_ptr());
    let Some(Expr::Literal(Value::Str(default))) = &source[0].default else {
        panic!("expected a literal default");
    };
    let expected = size_of::<Vec<ColumnDef>>()
        + source.capacity() * size_of::<ColumnDef>()
        + source[0].name.capacity()
        + default.capacity()
        + size_of::<Budgeted<Arc<Vec<ColumnDef>>>>();
    assert_eq!(retained.reserved_bytes(), expected);
    assert_eq!(budget.used(), expected);
    let remainder = budget.reserve(budget.limit() - budget.used()).unwrap();
    let nested = retained.clone();
    assert_eq!(budget.used(), budget.limit());
    drop(remainder);
    drop(source);
    drop(retained);
    assert_eq!(budget.used(), expected);
    assert_eq!(nested[0].name, "v");
    drop(nested);
    assert_eq!(budget.used(), 0);
}

#[test]
fn quota_and_cancellation_leave_the_original_generation_unretained() {
    let mut source = columns("CREATE TABLE t(v text)");
    source[0].name = spare_text("v", 64 * 1024);
    let source = Arc::new(source);
    let budget = MemoryBudget::new(16 * 1024);
    let cancellation = CancellationToken::new();
    assert!(matches!(
        RetainedColumns::capture(&source, &budget, &cancellation),
        Err(CatalogRetentionError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(Arc::strong_count(&source), 1);
    assert_eq!(budget.used(), 0);
    assert!(budget.peak() > 0);
    cancellation.cancel();
    assert!(matches!(
        RetainedColumns::capture(&source, &budget, &cancellation),
        Err(CatalogRetentionError::Cancelled(_))
    ));
    assert_eq!(Arc::strong_count(&source), 1);
    assert_eq!(budget.used(), 0);
}

#[test]
fn column_types_charge_domain_names_and_every_boxed_base() {
    let schema = spare_text("schema", 513);
    let name = spare_text("domain", 1027);
    let expected = schema.capacity() + name.capacity() + 2 * size_of::<ColumnType>();
    let ty = ColumnType::Domain {
        schema,
        name,
        oid: 42,
        base: Box::new(ColumnType::Array(Box::new(ColumnType::Text))),
    };
    let budget = MemoryBudget::new(16 * 1024);
    let memory = ty
        .reserve_retained_payload(&budget, &CancellationToken::new())
        .unwrap();
    assert_eq!(memory.bytes(), expected);
    assert_eq!(budget.used(), expected);
    drop(memory);
    assert_eq!(budget.used(), 0);
}

#[test]
fn column_literals_reuse_core_value_ownership_including_spare_capacity() {
    let mut fields = Vec::with_capacity(9);
    fields.push((
        spare_text("name", 257),
        Value::Str(spare_text("value", 1025)),
    ));
    let mut values = Vec::with_capacity(11);
    values.push(Value::Record(fields));
    let value = Value::List(values);
    let budget = MemoryBudget::new(64 * 1024);
    let cancellation = CancellationToken::new();
    let expected = value
        .retained_payload_bytes(&budget, &cancellation)
        .unwrap();
    let mut source = columns("CREATE TABLE t(v text)");
    source[0].missing_value = Some(value);
    let expected = expected + source[0].name.capacity();
    let memory = source[0]
        .reserve_retained_payload(&budget, &cancellation)
        .unwrap();
    assert_eq!(memory.bytes(), expected);
    drop(memory);
    assert_eq!(budget.used(), 0);
}

#[test]
fn constraint_names_and_sequence_owners_charge_their_actual_string_capacities() {
    let mut source = columns("CREATE TABLE t(v integer REFERENCES p(id))");
    let column = &mut source[0];
    column.not_null_name = Some(spare_text("nn", 1027));
    column.check_name = Some(spare_text("ck", 2053));
    column.auto_increment = Some(AutoIncrement {
        kind: AutoIncrementKind::Serial,
        sequence: Some(spare_text("seq", 4099)),
        owner: Some(AutoIncrementOwner {
            table: spare_text("owner", 8219),
            column: spare_text("id", 16411),
        }),
    });
    let reference = column.references.as_mut().unwrap();
    reference.name = Some(spare_text("fk", 32771));
    reference.referenced_key = Some(spare_text("pk", 65537));
    let sequence = column.auto_increment.as_ref().unwrap();
    let owner = sequence.owner.as_ref().unwrap();
    let expected: usize = [
        &column.name,
        column.not_null_name.as_ref().unwrap(),
        column.check_name.as_ref().unwrap(),
        sequence.sequence.as_ref().unwrap(),
        &owner.table,
        &owner.column,
        &reference.table,
        reference.column.as_ref().unwrap(),
        reference.name.as_ref().unwrap(),
        reference.referenced_key.as_ref().unwrap(),
    ]
    .into_iter()
    .map(String::capacity)
    .sum();
    let budget = MemoryBudget::new(1024 * 1024);
    let memory = column
        .reserve_retained_payload(&budget, &CancellationToken::new())
        .unwrap();
    assert_eq!(memory.bytes(), expected);
    drop(memory);
    assert_eq!(budget.used(), 0);
}

#[test]
fn generated_dependencies_retain_invocation_and_resolution_error_payloads() {
    let mut binding = FunctionBinding::undefined_function("f", spare_text("f(text)", 8193));
    let mut sources = Vec::with_capacity(5);
    sources.push(Some(spare_text("text", 1025)));
    let invocation = RoutineInvocationBinding {
        argument_positions: Vec::with_capacity(7),
        argument_targets: vec![spare_text("text", 259)],
        argument_sources: sources,
        parameter_types: vec![spare_text("text", 521)],
        return_type: Some(spare_text("text", 2051)),
        variadic_mode: RoutineVariadicMode::None,
    };
    let mut expected = binding.name.capacity()
        + match binding.resolution_error.as_ref().unwrap() {
            FunctionResolutionError::UndefinedFunction { signature } => signature.capacity(),
            FunctionResolutionError::Operator(_) => unreachable!(),
        }
        + size_of::<RoutineInvocationBinding>()
        + invocation.argument_positions.capacity() * size_of::<usize>()
        + invocation.argument_sources.capacity() * size_of::<Option<String>>()
        + invocation.argument_sources[0].as_ref().unwrap().capacity()
        + invocation.return_type.as_ref().unwrap().capacity();
    for names in [&invocation.argument_targets, &invocation.parameter_types] {
        expected += names.capacity() * size_of::<String>() + names[0].capacity();
    }
    binding.invocation = Some(Box::new(invocation));
    let mut dependencies = Vec::with_capacity(3);
    dependencies.push(binding);
    expected += dependencies.capacity() * size_of::<FunctionBinding>() + size_of::<Expr>();
    let mut source = columns("CREATE TABLE t(v integer)");
    source[0].generated = Some(GeneratedColumn {
        kind: GeneratedColumnKind::Stored,
        expression: Box::new(Expr::Literal(Value::Int(1))),
        function_dependencies: dependencies,
    });
    expected += source[0].name.capacity();
    let budget = MemoryBudget::new(128 * 1024);
    let memory = source[0]
        .reserve_retained_payload(&budget, &CancellationToken::new())
        .unwrap();
    assert_eq!(memory.bytes(), expected);
    drop(memory);
    assert_eq!(budget.used(), 0);
}

#[test]
fn deep_column_expression_walks_use_bounded_heap_scratch_and_release_it() {
    let depth = 4096;
    let mut expression = Expr::Literal(Value::Bool(true));
    for _ in 0..depth {
        expression = Expr::Not(Box::new(expression));
    }
    let budget = MemoryBudget::new(depth * size_of::<Expr>() + 4096);
    let memory = expression
        .reserve_column_payload(&budget, &CancellationToken::new())
        .unwrap();
    assert_eq!(memory.bytes(), depth * size_of::<Expr>());
    assert_eq!(budget.used(), memory.bytes());
    drop(memory);
    assert_eq!(budget.used(), 0);
    // Dispose the fixture iteratively too; its ordinary recursive destructor is unrelated to admission.
    while let Expr::Not(inner) = expression {
        expression = *inner;
    }
}

#[test]
fn traversal_capacity_cannot_escape_the_column_allowance() {
    let mut values = Vec::with_capacity(4096);
    values.resize(4096, Expr::Literal(Value::Int(1)));
    let payload = values.capacity() * size_of::<Expr>();
    let expression = Expr::Array(values);
    let budget = MemoryBudget::new(payload);
    assert!(matches!(
        expression.reserve_column_payload(&budget, &CancellationToken::new()),
        Err(CatalogRetentionError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn impossible_subquery_shapes_fail_without_retaining_partial_generations() {
    for sql in [
        "SELECT (SELECT 1)",
        "SELECT EXISTS (SELECT 1)",
        "SELECT 1 IN (SELECT 1)",
    ] {
        let Statement::Select(mut query) = crate::compile(sql).unwrap().remove(0) else {
            panic!("expected a SELECT");
        };
        let mut source = columns("CREATE TABLE t(v integer)");
        source[0].default = Some(Expr::Array(vec![query.projections.remove(0).expr]));
        let source = Arc::new(source);
        let budget = MemoryBudget::new(1024 * 1024);
        assert!(matches!(
            RetainedColumns::capture(&source, &budget, &CancellationToken::new()),
            Err(CatalogRetentionError::UnexpectedSubquery)
        ));
        assert_eq!(Arc::strong_count(&source), 1);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn catalog_admission_errors_preserve_memory_cancellation_and_invariant_sqlstates() {
    for (error, expected) in [
        (
            CatalogRetentionError::Memory(MemoryError::SizeOverflow),
            "53200",
        ),
        (
            CatalogRetentionError::from(ValueRetentionError::Memory(MemoryError::SizeOverflow)),
            "53200",
        ),
        (
            CatalogRetentionError::from(ValueRetentionError::Cancelled(QueryCancelled)),
            "57014",
        ),
        (CatalogRetentionError::UnexpectedSubquery, "XX000"),
    ] {
        assert_eq!(crate::SQLError::from(error).sqlstate(), Some(expected));
    }
}
