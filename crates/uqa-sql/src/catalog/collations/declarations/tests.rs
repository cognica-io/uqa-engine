//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::FunctionBinding;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

struct Builtins;
impl FunctionTypeResolver for Builtins {
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
}

#[test]
fn declarations_resolve_names_before_rejecting_noncollatable_types() {
    // PostgreSQL 18.4 GetColumnDefCollation observations, independently captured with the provider declaration reference.
    for (name, state, message) in [
        (
            "\"C\"",
            "42804",
            "collations are not supported by type integer",
        ),
        (
            "missing",
            "42704",
            "collation \"missing\" for encoding \"UTF8\" does not exist",
        ),
        (
            "missing.\"C\"",
            "3F000",
            "schema \"missing\" does not exist",
        ),
    ] {
        let error = select(&Builtins, &ColumnType::Integer, Some(name)).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state));
        assert_eq!(error.to_string(), message);
    }
}

#[test]
fn domain_collation_copies_preserve_identity_with_exact_admission() {
    let mut ty = ColumnType::Text;
    let mut bytes = size_of::<ColumnType>(); // The outer array's boxed element.
    for index in 0..16 {
        let schema = "custom".to_string();
        let name = format!("domain_{index}");
        bytes += schema.len() + name.len() + size_of::<ColumnType>();
        ty = ColumnType::Domain {
            schema,
            name,
            oid: 40000 + index,
            array_oid: None,
            collation_oid: Some(if index % 2 == 0 { 950 } else { 951 }),
            base: Box::new(ty),
        };
    }
    let ty = ColumnType::Array(Box::new(ty));
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(bytes);
    let control = ProductionControl::new(&budget, &token, &token);
    let copy = ty.clone_with_control(&control).unwrap();
    assert_eq!(*copy, ty);
    assert_eq!(
        crate::catalog::type_metadata::pg_type_collation_oid(&copy),
        951
    );
    assert_eq!(copy.reserved_bytes(), bytes);
    assert_eq!(budget.peak(), bytes);
    let mut column = ColumnDef::nullable("value", ty.clone());
    assert_eq!(column_oid(&column), 951);
    column.collation_oid = Some(950);
    assert_eq!(column_oid(&column), 950);
    drop(copy);
    assert_eq!(budget.used(), 0);
    let insufficient = MemoryBudget::new(bytes - 1);
    let control = ProductionControl::new(&insufficient, &token, &token);
    let error: SQLError = ty.clone_with_control(&control).unwrap_err().into();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(insufficient.used(), 0);
    assert!(insufficient.peak() < bytes);
}
