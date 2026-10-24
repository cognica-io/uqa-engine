//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{
    CompositeTypeReference, DeferredSQLError, EnumTypeReference, FunctionBinding, RangeAlignment,
    RangeTypeReference,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::{memory::MemoryBudget, CancellationToken, DatumValue};

struct Catalog<'a> {
    identity: String,
    current: Option<ColumnType>,
    error: Option<DeferredSQLError>,
    cancel: Option<&'a CancellationToken>,
    calls: AtomicUsize,
}

impl FunctionTypeResolver for Catalog<'_> {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        panic!("binding a retained type must not execute its input routines")
    }

    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        assert_eq!(name, self.identity);
        self.calls.fetch_add(1, Ordering::Relaxed);
        if let Some(token) = self.cancel {
            token.cancel();
        }
        if let Some(error) = &self.error {
            return Err(SQLError::Routine {
                sqlstate: error.sqlstate.clone(),
                message: error.message.clone(),
            });
        }
        Ok(self.current.clone())
    }

    fn require_type_usage(&self, _: &ColumnType) -> Result<(), SQLError> {
        panic!("binding does not declare a catalog object")
    }
}

fn types(schema: &str, name: &str) -> Vec<ColumnType> {
    let range = RangeTypeReference {
        schema: schema.into(),
        name: name.into(),
        oid: 42000,
        array_oid: 42001,
        alignment: RangeAlignment::Integer,
    };
    let scalar = [
        ColumnType::Enum(EnumTypeReference {
            schema: schema.into(),
            name: name.into(),
            oid: 42000,
            array_oid: 42001,
        }),
        ColumnType::Composite(CompositeTypeReference {
            schema: schema.into(),
            name: name.into(),
            oid: 42000,
            array_oid: 42001,
            relation_oid: 42002,
        }),
        ColumnType::Domain {
            schema: schema.into(),
            name: name.into(),
            oid: 42000,
            array_oid: Some(42001),
            collation_oid: None,
            base: Box::new(ColumnType::Integer),
        },
        ColumnType::DeclaredRange(range.clone()),
        ColumnType::DeclaredMultirange(range),
    ];
    scalar
        .into_iter()
        .flat_map(|ty| [ty.clone(), ColumnType::Array(Box::new(ty))])
        .collect()
}

fn catalog(ty: &ColumnType, current: Option<ColumnType>) -> Catalog<'static> {
    Catalog {
        identity: ty.user_type_identity().unwrap_or_default(),
        current,
        error: None,
        cancel: None,
        calls: AtomicUsize::new(0),
    }
}

#[test]
fn declared_parameters_resolve_current_names_without_reading_retained_bytes() {
    let control = ProductionControl::uncontrolled();
    for (old, current) in types("old", "before")
        .into_iter()
        .zip(types("new", "after"))
    {
        let catalog = catalog(&old, Some(current.clone()));
        let oid = crate::catalog::type_metadata::pg_type_oid(&old);
        for value in [
            Value::Null,
            Value::Datum(DatumValue::new(u32::try_from(oid).unwrap(), 100, vec![])),
        ] {
            let parameter = SQLParam::typed_scalar(value, old.clone());
            let resolved = bind_parameter_type_with_control(&parameter, &catalog, &control)
                .unwrap()
                .unwrap();
            // Reference equality compares immutable identity, so compare names as well.
            assert_eq!(format!("{:?}", &*resolved), format!("{current:?}"));
        }
        assert_eq!(catalog.calls.load(Ordering::Relaxed), 2);
    }
}

#[test]
fn missing_declarations_report_the_original_outer_parameter_oid() {
    let control = ProductionControl::uncontrolled();
    for ty in types("gone", "dropped") {
        for reported_missing in [false, true] {
            let mut catalog = catalog(&ty, None);
            if reported_missing {
                catalog.error = Some(DeferredSQLError {
                    sqlstate: "42704".into(),
                    message: "type with OID 42000 does not exist".into(),
                });
            }
            let oid = if matches!(ty, ColumnType::Array(_)) {
                42001
            } else {
                42000
            };
            let error = bind_parameter_type_with_control(
                &SQLParam::typed_scalar(Value::Null, ty.clone()),
                &catalog,
                &control,
            )
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some("XX000"));
            assert_eq!(
                error.to_string(),
                format!("cache lookup failed for type {oid}")
            );
        }
    }
}

#[test]
fn prepared_declarations_bind_independently_of_argument_values() {
    let old = types("old", "before").remove(0);
    let current = types("new", "after").remove(0);
    let declarations = [None, Some(ColumnType::Integer), Some(old.clone())];
    let mut catalog = catalog(&old, Some(current.clone()));
    let resolved = bind_declared_parameter_types(&declarations, &catalog).unwrap();
    assert_eq!(
        format!("{resolved:?}"),
        format!("{:?}", [None, Some(ColumnType::Integer), Some(current)])
    );
    assert_eq!(catalog.calls.load(Ordering::Relaxed), 1);
    catalog.current = None;
    let error = bind_declared_parameter_types(&declarations, &catalog).unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert_eq!(error.to_string(), "cache lookup failed for type 42000");
}

#[test]
fn procedural_errors_and_anonymous_values_keep_their_observation_boundary() {
    let control = ProductionControl::uncontrolled();
    let ty = types("gone", "dropped").remove(0);
    let catalog = catalog(&ty, None);
    let admitted = SQLParam::typed_scalar(Value::Null, ty.clone());
    assert_eq!(
        crate::scalar_type_with_resolver(
            &ScalarExpr::Param(1),
            &RowSchema::default(),
            &[admitted],
            &catalog,
        )
        .unwrap(),
        Some(ty.clone()),
    );
    let deferred = SQLParam::DeferredError {
        ty: ty.clone(),
        error: DeferredSQLError {
            sqlstate: "XX001".into(),
            message: "unobserved field".into(),
        },
    };
    assert_eq!(
        *bind_parameter_type_with_control(&deferred, &catalog, &control)
            .unwrap()
            .unwrap(),
        ty
    );
    for ty in [
        ColumnType::Record,
        ColumnType::AnyArray,
        ColumnType::Integer,
    ] {
        let parameter = SQLParam::typed_scalar(
            Value::Datum(DatumValue::new(42000, 100, vec![])),
            ty.clone(),
        );
        assert_eq!(
            *bind_parameter_type_with_control(&parameter, &catalog, &control)
                .unwrap()
                .unwrap(),
            ty
        );
    }
    assert_eq!(catalog.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn catalog_binding_retains_allowance_and_both_cancellation_sources() {
    let ty = types("old", "before").remove(0);
    let current = types("new", "after").remove(0);
    let parameter = SQLParam::typed_scalar(Value::Null, ty.clone());
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let budget = MemoryBudget::new(4096);
    let control = ProductionControl::new(&budget, &original, &invoking);
    let held = control.copy_text("held").unwrap();
    let mut catalog = catalog(&ty, Some(current));
    let value = bind_parameter_type_with_control(&parameter, &catalog, &control)
        .unwrap()
        .unwrap();
    assert!(value.reserved_bytes() > 0);
    assert_eq!(
        budget.used(),
        held.reserved_bytes() + value.reserved_bytes()
    );
    drop(value);
    for token in [&original, &invoking] {
        catalog.cancel = Some(token);
        let error = bind_parameter_type_with_control(&parameter, &catalog, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), held.reserved_bytes());
        token.reset();
    }
    catalog.cancel = None;
    catalog.current = Some(types("new", &"x".repeat(8192)).remove(0));
    let error = bind_parameter_type_with_control(&parameter, &catalog, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), held.reserved_bytes());
    catalog.error = Some(DeferredSQLError {
        sqlstate: "40001".into(),
        message: "catalog snapshot changed".into(),
    });
    let error = bind_parameter_type_with_control(&parameter, &catalog, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("40001"));
    assert_eq!(error.to_string(), "catalog snapshot changed");
    assert_eq!(budget.used(), held.reserved_bytes());
}
