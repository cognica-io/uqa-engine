//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluate retained row positions against the current composite descriptor.

use crate::{
    ast::{ColumnType, CompositeRowBinding},
    expr::EngineHook,
    SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

mod state;
pub use state::CompositeConstructorState;

pub fn evaluate_with_control(
    binding: &CompositeRowBinding,
    arguments: usize,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
    evaluate: impl FnMut(usize) -> Result<Produced<Value>, SQLError>,
) -> Result<Produced<Value>, SQLError> {
    evaluate_with_state(binding, arguments, engine, control, None, evaluate)
}

pub fn evaluate_with_state(
    binding: &CompositeRowBinding,
    arguments: usize,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
    state: Option<&CompositeConstructorState>,
    evaluate: impl FnMut(usize) -> Result<Produced<Value>, SQLError>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let engine =
        engine.ok_or_else(|| SQLError::Internal("composite constructor has no catalog".into()))?;
    let resolved = engine
        .resolve_type_name(&binding.ty)
        .map_err(SQLError::Internal)?
        .map(|ty| ty.retain_external_with_control(control))
        .transpose()?;
    control.check()?;
    let Some(ColumnType::Composite(reference)) = resolved.as_deref() else {
        return Err(SQLError::Internal(
            "composite constructor type disappeared".into(),
        ));
    };
    let descriptor = super::descriptor(engine.composite_types(), reference.oid)?;
    let value = construct_with_state(binding, arguments, &descriptor, control, state, evaluate)?;
    let Value::Record(record) = &*value else {
        unreachable!("named composite producer");
    };
    super::retention::retain_record(record, &descriptor, engine.composite_types(), control)
}

/// Form a row using a resolved descriptor, after checking all surviving argument types and before evaluating any argument.
pub fn construct_with_control(
    binding: &CompositeRowBinding,
    arguments: usize,
    descriptor: &super::CompositeTypeDescriptor,
    control: &ProductionControl<'_>,
    evaluate: impl FnMut(usize) -> Result<Produced<Value>, SQLError>,
) -> Result<Produced<Value>, SQLError> {
    construct_with_state(binding, arguments, descriptor, control, None, evaluate)
}

pub fn construct_with_state(
    binding: &CompositeRowBinding,
    arguments: usize,
    descriptor: &super::CompositeTypeDescriptor,
    control: &ProductionControl<'_>,
    state: Option<&CompositeConstructorState>,
    mut evaluate: impl FnMut(usize) -> Result<Produced<Value>, SQLError>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    if binding.attributes.len() != arguments
        || binding.attributes.iter().any(|number| *number <= 0)
        || binding.attributes.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(SQLError::Internal(
            "invalid stored composite constructor positions".into(),
        ));
    }
    if let Some(types) = &binding.argument_types {
        if types.len() != arguments {
            return Err(SQLError::Internal(
                "invalid stored composite constructor types".into(),
            ));
        }
        for attribute in &descriptor.attributes {
            control.check()?;
            if let Ok(index) = binding.attributes.binary_search(&attribute.number) {
                let source = &types[index];
                if crate::catalog::type_metadata::pg_type_oid(source)
                    != crate::catalog::type_metadata::pg_type_oid(&attribute.ty)
                {
                    return Err(SQLError::Routine {
                        sqlstate: "42804".into(),
                        message: format!(
                            "ROW() column has type {} instead of type {}",
                            source.display_name(),
                            attribute.ty.display_name()
                        ),
                    });
                }
            }
        }
    }
    produce(descriptor, control, state, |_, attribute| {
        match binding.attributes.binary_search(&attribute.number) {
            Ok(index) => evaluate(index),
            Err(_) => Ok(control.finish(Value::Null, control.empty_reservation())?),
        }
    })
}

/// Construct a newly admitted record from exactly the current live attributes. The caller binds and coerces each positional input; no stored-expression binding is required.
pub fn construct_current_with_control(
    descriptor: &super::CompositeTypeDescriptor,
    catalog: Option<&dyn super::CompositeTypeCatalog>,
    control: &ProductionControl<'_>,
    mut evaluate: impl FnMut(usize) -> Result<Produced<Value>, SQLError>,
) -> Result<Produced<Value>, SQLError> {
    let value = produce(descriptor, control, None, |index, _| evaluate(index))?;
    let Value::Record(record) = &*value else {
        unreachable!("named composite producer")
    };
    super::retention::retain_record(record, descriptor, catalog, control)
}

fn produce(
    descriptor: &super::CompositeTypeDescriptor,
    control: &ProductionControl<'_>,
    state: Option<&CompositeConstructorState>,
    mut evaluate: impl FnMut(usize, &super::CompositeAttribute) -> Result<Produced<Value>, SQLError>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let shared = state.and_then(|state| state.shared(descriptor));
    let mut names = ProductionVec::new(*control);
    let mut values = ProductionVec::new(*control);
    values.reserve(descriptor.attributes.len())?;
    if shared.is_none() {
        names.reserve(descriptor.attributes.len())?;
    }
    for (index, attribute) in descriptor.attributes.iter().enumerate() {
        let value = evaluate(index, attribute)?;
        if shared.is_none() {
            names.push_produced(control.copy_text(&attribute.name)?)?;
        }
        values.push_produced(value)?;
    }
    let record = if let Some(shared) = shared {
        uqa_core::RecordValue::with_shared_control(values.finish()?, shared, control)?
    } else {
        let descriptor = uqa_core::RecordDescriptor::with_control(
            names.finish()?,
            Some(descriptor.type_oid),
            control,
        )?;
        uqa_core::RecordValue::with_descriptor_control(values.finish()?, descriptor, control)?
    };
    if let Some(state) = state {
        state.remember(record.descriptor(), control)?;
    }
    let (record, memory) = record.into_parts();
    control
        .finish(Value::Record(record), memory)
        .map_err(Into::into)
}

/// Bind predecessor constructor metadata while its original descriptor is still current. Attribute removal cannot make a discarded argument run again.
pub fn retain_argument_types(
    binding: &mut CompositeRowBinding,
    types: &dyn crate::type_resolution::FunctionTypeResolver,
    catalog: Option<&dyn super::CompositeTypeCatalog>,
) -> Result<bool, SQLError> {
    if binding.argument_types.is_some() {
        return Ok(false);
    }
    let Some(ColumnType::Composite(reference)) = types.resolve_type_name(&binding.ty)? else {
        return Err(SQLError::Internal(
            "composite constructor type disappeared".into(),
        ));
    };
    let descriptor = super::descriptor(catalog, reference.oid)?;
    binding.argument_types = Some(
        binding
            .attributes
            .iter()
            .map(|number| {
                descriptor
                    .attributes
                    .iter()
                    .find(|attribute| attribute.number == *number)
                    .map_or(ColumnType::Integer, |attribute| attribute.ty.clone())
            })
            .collect(),
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::CompositeTypeReference;
    use crate::expr::composites::{
        CompositeAttribute, CompositeTypeCatalog, CompositeTypeDescriptor,
    };
    use std::sync::Arc;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    struct Catalog;
    fn reference() -> ColumnType {
        ColumnType::Composite(CompositeTypeReference {
            schema: "public".into(),
            name: "pair".into(),
            oid: 20_001,
            array_oid: 20_002,
            relation_oid: 20_003,
        })
    }
    impl CompositeTypeCatalog for Catalog {
        fn composite_type(&self, _: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
            Ok(Some(Arc::new(CompositeTypeDescriptor {
                dropped: Vec::new(),
                type_oid: 20_001,
                relation_oid: 20_003,
                attributes: [1, 3, 4]
                    .into_iter()
                    .map(|number| CompositeAttribute {
                        collation_oid: None,
                        name: format!("a{number}"),
                        ty: ColumnType::Integer,
                        number,
                    })
                    .collect(),
            })))
        }
    }
    impl EngineHook for Catalog {
        fn nextval(&self, _: &str) -> Result<i64, SQLError> {
            unreachable!()
        }
        fn currval(&self, _: &str) -> Result<i64, SQLError> {
            unreachable!()
        }
        fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
            unreachable!()
        }
        fn resolve_type_name(&self, _: &str) -> Result<Option<ColumnType>, String> {
            Ok(Some(reference()))
        }
        fn composite_types(&self) -> Option<&dyn CompositeTypeCatalog> {
            Some(self)
        }
    }

    #[test]
    fn stored_positions_extend_nulls_without_evaluating_removed_fields() {
        let budget = MemoryBudget::new(4096);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let binding = CompositeRowBinding {
            layout: None,
            argument_types: Some(vec![
                ColumnType::Integer,
                ColumnType::Text,
                ColumnType::Integer,
            ]),
            ty: "composite#20001".into(),
            attributes: vec![1, 2, 3],
        };
        let mut evaluated = Vec::new();
        let value = evaluate_with_control(&binding, 3, Some(&Catalog), &control, |index| {
            evaluated.push(index);
            control
                .finish(Value::Int(index as i64), control.empty_reservation())
                .map_err(Into::into)
        })
        .unwrap();
        assert_eq!(evaluated, vec![0, 2]);
        let Value::Datum(datum) = &*value else {
            panic!("retained constructor output");
        };
        assert_eq!(datum.type_oid(), 20_001);
        for (name, expected) in [
            ("a1", Value::Int(0)),
            ("a3", Value::Int(2)),
            ("a4", Value::Null),
        ] {
            let field = crate::expr::datums::read_composite_field_with_control(
                datum,
                name,
                Some(&Catalog),
                &control,
            )
            .unwrap();
            let decoded = match &*field {
                Value::Datum(datum) => {
                    crate::expr::datums::read_with_control(datum, &control).unwrap()
                }
                value => control.copy_value(value).unwrap(),
            };
            assert_eq!(*decoded, expected);
        }
        assert!(budget.used() > 0);
        drop(value);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn retained_argument_types_are_checked_before_any_argument_effect() {
        let control = ProductionControl::uncontrolled();
        let binding = CompositeRowBinding {
            layout: None,
            ty: "composite#20001".into(),
            attributes: vec![1, 2, 3],
            argument_types: Some(vec![
                ColumnType::Integer,
                ColumnType::Text,
                ColumnType::Real,
            ]),
        };
        let error = evaluate_with_control(&binding, 3, Some(&Catalog), &control, |_| {
            panic!("a type mismatch must precede every argument effect")
        })
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42804"));
        assert_eq!(
            error.to_string(),
            "ROW() column has type real instead of type integer"
        );
    }

    #[test]
    fn constructor_checks_cancellation_and_admission_before_evaluating_arguments() {
        let budget = MemoryBudget::new(0);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let binding = CompositeRowBinding {
            layout: None,
            argument_types: None,
            ty: "composite#20001".into(),
            attributes: vec![1],
        };
        let error =
            evaluate_with_control(&binding, 1, Some(&Catalog), &control, |_| unreachable!())
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        cancellation.cancel();
        let error =
            evaluate_with_control(&binding, 1, Some(&Catalog), &control, |_| unreachable!())
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
    }
    #[test]
    fn composite_only_embedding_keeps_comparison_metadata_without_enum_support() {
        let value = Value::Record(uqa_core::RecordValue::from_parts(
            vec![
                ("a1".into(), Value::Int(1)),
                ("a3".into(), Value::Int(2)),
                ("a4".into(), Value::Null),
            ],
            Some(20_001),
        ));
        let expression = crate::ast::Expr::Binary {
            op: crate::ast::BinaryOp::Equal,
            lhs: Box::new(crate::ast::Expr::Literal(value.clone())),
            rhs: Box::new(crate::ast::Expr::Literal(value)),
        };
        assert!(Catalog.enum_labels().is_none());
        let context = crate::expr::EvalContext::new(None, &[]).with_engine(&Catalog);
        assert_eq!(
            crate::expr::eval(&expression, &context).unwrap(),
            Value::Bool(true)
        );
    }
    #[test]
    fn repeated_composite_constructors_share_names_with_live_descriptor_validation() {
        let budget = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let state = CompositeConstructorState::default();
        let mut descriptor = (*Catalog.composite_type(20_001).unwrap().unwrap()).clone();
        for attribute in &mut descriptor.attributes {
            attribute.name = format!("{}{}", attribute.number, "n".repeat(8192));
        }
        let binding = CompositeRowBinding {
            layout: None,
            ty: "composite#20001".into(),
            attributes: vec![1, 3, 4],
            argument_types: Some(vec![ColumnType::Integer; 3]),
        };
        let make = |descriptor: &CompositeTypeDescriptor| {
            construct_with_state(&binding, 3, descriptor, &control, Some(&state), |index| {
                Ok(control.finish(Value::Int(index as i64), control.empty_reservation())?)
            })
        };
        let first = make(&descriptor).unwrap();
        let Value::Record(first_record) = &*first else {
            panic!("record")
        };
        let mut last = None;
        let allocations = allocation_counter::measure(|| {
            last = Some(make(&descriptor).unwrap());
        });
        let last = last.unwrap();
        let Value::Record(last_record) = &*last else {
            panic!("record")
        };
        assert!(first_record
            .descriptor()
            .shares_storage_with(last_record.descriptor()));
        assert!(
            allocations.bytes_total < 4096,
            "warm construction must not copy long names: {allocations:?}"
        );
        assert_eq!(*first, *last);
        descriptor.attributes[0].name = "renamed".into();
        let renamed = make(&descriptor).unwrap();
        let Value::Record(renamed_record) = &*renamed else {
            panic!("record")
        };
        assert!(!first_record
            .descriptor()
            .shares_storage_with(renamed_record.descriptor()));
        assert_eq!(renamed_record.descriptor().field_names()[0], "renamed");
        descriptor.attributes[0].ty = ColumnType::Text;
        let error = construct_with_state(&binding, 3, &descriptor, &control, Some(&state), |_| {
            panic!("live validation precedes argument evaluation")
        })
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42804"));
        drop((first, renamed, state));
        assert_eq!(budget.used(), last.reserved_bytes());
        drop(last);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn composite_descriptor_reuse_preserves_drop_add_type_identity_and_cancellation() {
        let control = ProductionControl::uncontrolled();
        let state = CompositeConstructorState::default();
        let binding = CompositeRowBinding {
            layout: None,
            ty: "composite#20001".into(),
            attributes: vec![1, 2, 3],
            argument_types: Some(vec![ColumnType::Integer; 3]),
        };
        let mut descriptor = (*Catalog.composite_type(20_001).unwrap().unwrap()).clone();
        let evaluate = |_: usize| Ok(control.finish(Value::Int(1), control.empty_reservation())?);
        let first =
            construct_with_state(&binding, 3, &descriptor, &control, Some(&state), evaluate)
                .unwrap();
        descriptor.type_oid = 20_009;
        let mut calls = Vec::new();
        let output =
            construct_with_state(&binding, 3, &descriptor, &control, Some(&state), |index| {
                calls.push(index);
                evaluate(index)
            })
            .unwrap();
        assert_eq!(calls, [0, 2]);
        let Value::Record(record) = &*output else {
            panic!("record")
        };
        assert_eq!(record.type_oid(), Some(20_009));
        assert_eq!(record.values()[2], Value::Null);
        assert!(!first.has_same_representation(&output));
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let controlled = ProductionControl::new(&budget, &token, &token);
        token.cancel();
        assert_eq!(
            construct_with_state(
                &binding,
                3,
                &descriptor,
                &controlled,
                Some(&state),
                |_| panic!("cancelled")
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}
