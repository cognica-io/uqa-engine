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

pub fn evaluate_with_control(
    binding: &CompositeRowBinding,
    arguments: usize,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
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
    let mut fields = ProductionVec::new(*control);
    fields.reserve(descriptor.attributes.len())?;
    for attribute in &descriptor.attributes {
        let value = match binding.attributes.binary_search(&attribute.number) {
            Ok(index) => evaluate(index)?,
            Err(_) => control.finish(Value::Null, control.empty_reservation())?,
        };
        let name = control.copy_text(&attribute.name)?;
        let (value, value_memory) = value.into_parts();
        let (name, name_memory) = name.into_parts();
        fields.push_produced(
            control.finish((name, value), control.combine(name_memory, value_memory))?,
        )?;
    }
    let (fields, memory) = fields.finish()?.into_parts();
    control
        .finish(Value::Record(fields), memory)
        .map_err(Into::into)
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
                type_oid: 20_001,
                relation_oid: 20_003,
                attributes: [1, 3, 4]
                    .into_iter()
                    .map(|number| CompositeAttribute {
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
        assert_eq!(
            *value,
            Value::Record(vec![
                ("a1".into(), Value::Int(0)),
                ("a3".into(), Value::Int(2)),
                ("a4".into(), Value::Null)
            ])
        );
        assert!(budget.used() > 0);
        drop(value);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn constructor_checks_cancellation_and_admission_before_evaluating_arguments() {
        let budget = MemoryBudget::new(0);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let binding = CompositeRowBinding {
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
}
