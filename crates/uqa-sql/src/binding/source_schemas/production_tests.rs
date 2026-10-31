//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{JoinKind, JoinUsing},
    binding::fixture::{empty_binding_context, NoRoutines},
};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

#[test]
fn controlled_join_output_binding_rejects_wide_metadata_before_copying() {
    let input = |qualifier| {
        RowSchema::with_qualified_types(
            qualifier,
            (0..256)
                .map(|position| format!("{position}_{}", "x".repeat(1024)))
                .collect(),
            vec![Some(ColumnType::Integer); 256],
        )
    };
    let left = input("l");
    let right = input("r");
    let using = JoinUsing {
        columns: vec![left.columns()[0].clone()],
        alias: Some("merged".into()),
    };
    let mut failures = Vec::new();
    for (kind, using, alias) in [
        (JoinKind::Cross, None, None),
        (JoinKind::Inner, Some(&using), None),
        (JoinKind::Full, Some(&using), None),
        (JoinKind::Inner, None, Some("joined")),
    ] {
        let memory = MemoryBudget::new(32 << 10);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let mut scope = SchemaScope::from_context_with_control(
            empty_binding_context().copy_with_control(&control).unwrap(),
            &control,
        )
        .unwrap();
        let baseline = memory.used();
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = Some(scope.bind_join_output_schema(JoinSchemaBinding {
                routines: &NoRoutines,
                kind,
                on: None,
                using,
                natural: false,
                alias,
                column_aliases: &[],
                left: &left,
                right: &right,
                subqueries: &[],
                params: &[],
                outer: None,
            }));
        });
        let error = result.unwrap().err();
        if error.as_ref().and_then(SQLError::sqlstate) != Some("53200")
            || allocation.bytes_total >= 128 << 10
        {
            failures.push(format!(
                "{kind:?}, alias={alias:?}: {error:?}; {allocation:?}"
            ));
        }
        assert_eq!(memory.used(), baseline);
        drop(scope);
        assert_eq!(memory.used(), 0);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn join_output_condition_binding_observes_both_tokens_and_retires_failed_layouts() {
    let left =
        RowSchema::with_qualified_types("l", vec!["id".into()], vec![Some(ColumnType::Integer)]);
    let right =
        RowSchema::with_qualified_types("r", vec!["id".into()], vec![Some(ColumnType::Integer)]);
    let condition = ScalarExpr::Literal(uqa_core::Value::Bool(true));
    for validate in [false, true] {
        let memory = MemoryBudget::new(4 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let mut scope = SchemaScope::from_context_with_control(
            empty_binding_context().copy_with_control(&control).unwrap(),
            &control,
        )
        .unwrap();
        scope.validate_references = validate;
        let baseline = memory.used();
        let binding = || JoinSchemaBinding {
            routines: &NoRoutines,
            kind: JoinKind::Inner,
            on: Some(&condition),
            using: None,
            natural: false,
            alias: Some("j"),
            column_aliases: &[],
            left: &left,
            right: &right,
            subqueries: &[],
            params: &[],
            outer: None,
        };
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(
                scope
                    .bind_join_output_schema(binding())
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(memory.used(), baseline);
            token.reset();
        }
        let occupied = memory.reserve(memory.available()).unwrap();
        assert_eq!(
            scope
                .bind_join_output_schema(binding())
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(memory.used(), baseline + occupied.bytes());
        drop(occupied);
        let output = scope.bind_join_output_schema(binding()).unwrap();
        assert_eq!(output.len(), 2);
        assert!(output.has_qualifier("j"));
        assert!(memory.used() > baseline);
        drop((output, scope));
        assert_eq!(memory.used(), 0);
    }
}

mod source_descriptors {
    use crate::{
        ast::{ColumnDef, InternalRelationId},
        binding::fixture::{
            catalog, catalog_with_foreign_tables, empty_binding_context, table_definition,
            NoRoutines,
        },
        ColumnType, RelationIdentity,
    };
    use crate::{
        binding::{BindingContext, SchemaScope},
        plan::SourcePlan,
        RowSchema, SQLError,
    };
    use std::collections::BTreeMap;
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };

    fn input(kind: usize) -> (SourcePlan, BindingContext<'static>) {
        let mut context = empty_binding_context();
        let wide = "x".repeat(1 << 20);
        let mut source = SourcePlan::Table {
            executable_query: None,
            bound_columns: None,
            name: "source".into(),
            qualifier: "source".into(),
            alias: None,
            column_aliases: Vec::new(),
            include_descendants: true,
        };
        match kind {
            0 | 1 => {
                let tables = BTreeMap::from([(
                    RelationIdentity::new("public", "source"),
                    table_definition(vec![ColumnDef::nullable(wide, ColumnType::Integer)]),
                )]);
                context.catalog = if kind == 0 {
                    catalog(tables)
                } else {
                    catalog_with_foreign_tables(BTreeMap::new(), tables)
                };
            }
            2 => {
                context.ctes.insert(
                    "source".into(),
                    RowSchema::with_types(vec!["value".into()], vec![Some(ColumnType::Integer)]),
                );
                let SourcePlan::Table { column_aliases, .. } = &mut source else {
                    unreachable!()
                };
                column_aliases.push(wide);
            }
            3 => {
                source = SourcePlan::Values {
                    lateral: false,
                    rows: Vec::new(),
                    alias: None,
                    column_aliases: Vec::new(),
                    internal_relation: Some(InternalRelationId::allocate()),
                    internal_column_types: vec![Some(ColumnType::Named(wide))],
                    internal_column_collations: Vec::new(),
                };
            }
            _ => unreachable!(),
        }
        (source, context)
    }

    #[test]
    fn controlled_source_descriptors_reject_wide_declarations_and_aliases_before_copying() {
        let mut failures = Vec::new();
        for (kind, label) in ["table", "foreign table", "CTE alias", "internal VALUES"]
            .iter()
            .enumerate()
        {
            let (source, context) = input(kind);
            let memory = MemoryBudget::new(32 << 10);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&memory, &original, &invoking);
            let mut scope = SchemaScope::from_context_with_control(
                context.copy_with_control(&control).unwrap(),
                &control,
            )
            .unwrap();
            let baseline = memory.used();
            let mut result = None;
            let allocation = allocation_counter::measure(|| {
                result = Some(scope.bind_source(&NoRoutines, &source, &[], &[], None));
            });
            let error = result.unwrap().err();
            if error.as_ref().and_then(SQLError::sqlstate) != Some("53200")
                || allocation.bytes_total >= 128 << 10
            {
                failures.push(format!("{label}: {error:?}; {allocation:?}"));
            }
            assert_eq!(memory.used(), baseline);
            drop(scope);
            assert_eq!(memory.used(), 0);
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
    #[test]
    fn source_descriptor_owners_survive_scope_retirement_and_both_cancellations() {
        for kind in 0..4 {
            let (source, context) = input(kind);
            let memory = MemoryBudget::new(64 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&memory, &original, &invoking);
            let mut scope = SchemaScope::from_context_with_control(
                context.copy_with_control(&control).unwrap(),
                &control,
            )
            .unwrap();
            let baseline = memory.used();
            for token in [&original, &invoking] {
                token.cancel();
                assert_eq!(
                    scope
                        .bind_source(&NoRoutines, &source, &[], &[], None)
                        .unwrap_err()
                        .sqlstate(),
                    Some("57014")
                );
                assert_eq!(memory.used(), baseline);
                token.reset();
            }
            let schema = scope
                .bind_source(&NoRoutines, &source, &[], &[], None)
                .unwrap();
            assert!(memory.used() >= baseline + (1 << 20));
            let retained = schema.clone();
            drop((scope, schema));
            assert!(memory.used() >= 1 << 20);
            drop(retained);
            assert_eq!(memory.used(), 0);
        }
    }
}
