//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{FunctionBinding, Statement},
    plan::ExpressionPlan,
    FunctionTypeResolver, SQLError, SQLParam, ScalarExpr,
};
use std::sync::atomic::{AtomicUsize, Ordering};

fn columns() -> Vec<ColumnDef> {
    let Statement::CreateTable(table) = crate::compile("CREATE TABLE t (a smallint, b real, label varchar(12), items integer[], \"t.name\" text, \"Mixed\" bigint)").unwrap().remove(0) else {
        panic!("table declaration");
    };
    table.columns
}

fn physical(columns: &[ColumnDef]) -> RowSchema {
    RowSchema::with_types(
        columns.iter().map(|column| column.name.clone()).collect(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    )
}

fn scalar(sql: &str) -> ScalarExpr {
    let Statement::Select(mut query) = crate::compile(sql).unwrap().remove(0) else {
        panic!("SELECT");
    };
    ExpressionPlan::lower(query.projections.remove(0).expr).scalar
}

#[test]
fn declared_lookup_borrows_exact_types_and_matches_unqualified_physical_schema() {
    let mut columns = columns();
    columns[3].ty = ColumnType::Array(Box::new(ColumnType::Domain {
        schema: "Quoted.Schema".into(),
        name: "Domain.Name".into(),
        oid: 123,
        base: Box::new(ColumnType::Numeric {
            precision: Some(7),
            scale: Some(2),
        }),
    }));
    let duplicate = columns[0].clone();
    columns.push(duplicate);
    let declared = ColumnTypeSchema::new(&columns);
    let physical = physical(&columns);
    for name in ["a", "b", "items", "t.name", "Mixed", "mixed", "absent"] {
        assert_eq!(
            declared.has_unqualified_column(name),
            physical.has_unqualified_column(name)
        );
        assert_eq!(
            declared.column_is_ambiguous(name),
            physical.column_is_ambiguous(name)
        );
        assert_eq!(declared.type_of(name), physical.type_of(name));
    }
    assert!(std::ptr::eq(
        declared.type_of("items").unwrap(),
        &columns[3].ty
    ));
    for position in 0..=columns.len() {
        assert_eq!(
            declared.column_type(position),
            physical.column_type(position)
        );
        if let Some(ty) = declared.column_type(position) {
            assert!(std::ptr::eq(ty, &columns[position].ty));
        }
    }
    for qualifier in ["t", "absent", ""] {
        assert_eq!(
            declared.has_qualifier(qualifier),
            physical.has_qualifier(qualifier)
        );
        assert_eq!(
            declared.has_qualified_column(qualifier, "name"),
            physical.has_qualified_column(qualifier, "name")
        );
        assert_eq!(
            declared.qualified_type(qualifier, "name"),
            physical.qualified_type(qualifier, "name")
        );
    }
    assert!(declared.physical_schema().is_none());
    assert!(std::ptr::eq(
        ScalarTypeSchema::physical_schema(&physical).unwrap(),
        &physical
    ));
}

#[test]
fn borrowed_generated_type_binding_matches_existing_schema_semantics() {
    let mut columns = columns();
    columns[0].ty = ColumnType::Domain {
        schema: "Typed.Schema".into(),
        name: "Small Domain".into(),
        oid: 456,
        base: Box::new(ColumnType::SmallInteger),
    };
    columns[3].ty = ColumnType::Array(Box::new(columns[0].ty.clone()));
    let declared = ColumnTypeSchema::new(&columns);
    let physical = physical(&columns);
    for sql in [
        "SELECT pg_typeof(a)",
        "SELECT pg_typeof(label)",
        "SELECT pg_typeof(items)",
        "SELECT a + b",
        "SELECT -a",
        "SELECT coalesce(a, 5)",
        "SELECT greatest(a, 10)",
        "SELECT ARRAY[a, 5]",
        "SELECT CASE WHEN a > 1 THEN label ELSE 'fallback' END",
        "SELECT CASE 1 WHEN 1 THEN a ELSE 10 END",
        "SELECT CAST(b AS text)",
        "SELECT array_reverse(items)",
        "SELECT array_sort(items, true)",
        "SELECT to_hex(a)",
        "SELECT upper(label)",
        "SELECT ROW(a, label)",
        "SELECT \"t.name\"",
        "SELECT \"Mixed\"",
        "SELECT t.name",
        "SELECT missing",
    ] {
        let source = scalar(sql);
        let borrowed = crate::bind_type_introspection(source.clone(), &declared, &[]);
        let owned = crate::bind_type_introspection(source.clone(), &physical, &[]);
        assert_eq!(borrowed, owned, "{sql}");
        let describe = |result: Result<Option<ColumnType>, SQLError>| {
            result.map_err(|error| (error.sqlstate().map(str::to_owned), error.to_string()))
        };
        assert_eq!(
            describe(crate::scalar_type(&source, &declared, &[])),
            describe(crate::scalar_type(&source, &physical, &[])),
            "{sql}"
        );
    }
}

struct Resolver<'a> {
    expected: &'a RowSchema,
    calls: AtomicUsize,
}

impl FunctionTypeResolver for Resolver<'_> {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }

    fn resolve_scalar_subquery_type(
        &self,
        slot: crate::SubqueryId,
        outer_schema: &RowSchema,
        _: &[SQLParam],
    ) -> Result<Option<ColumnType>, SQLError> {
        assert_eq!(slot, 7);
        assert!(std::ptr::eq(outer_schema, self.expected));
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(Some(ColumnType::Integer))
    }
}

#[test]
fn custom_subquery_resolvers_receive_the_original_physical_schema() {
    let columns = columns();
    let physical = physical(&columns);
    let resolver = Resolver {
        expected: &physical,
        calls: AtomicUsize::new(0),
    };
    assert_eq!(
        crate::scalar_type_with_resolver(&ScalarExpr::ScalarSubquery(7), &physical, &[], &resolver)
            .unwrap(),
        Some(ColumnType::Integer)
    );
    let membership = ScalarExpr::InSubquery {
        expr: Box::new(ScalarExpr::Literal(uqa_core::Value::Int(1))),
        subquery: 7,
        negated: false,
    };
    assert_eq!(
        crate::scalar_type_with_resolver(&membership, &physical, &[], &resolver).unwrap(),
        Some(ColumnType::Boolean)
    );
    assert_eq!(resolver.calls.load(Ordering::Relaxed), 2);
    let declared = ColumnTypeSchema::new(&columns);
    assert_eq!(
        crate::scalar_type_with_resolver(&ScalarExpr::ScalarSubquery(7), &declared, &[], &resolver)
            .unwrap(),
        None
    );
    assert_eq!(resolver.calls.load(Ordering::Relaxed), 2);
}
