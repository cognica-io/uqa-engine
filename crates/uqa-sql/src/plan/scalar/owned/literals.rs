//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Literal metadata replacement preserves original composite provenance and producer ownership.

use super::{OwnedScalarExpr, Result, ScalarExpr, ScalarMemory};
use crate::{
    plan::scalar::{
        resources::{Control, Lowering},
        source::Source,
    },
    ColumnType,
};
use uqa_core::memory::{BudgetedVec, Produced, ProductionControl};
use uqa_core::Value;

impl OwnedScalarExpr {
    /// Consume an untyped literal while keeping its exact value payload lease. No metadata or child allocation belongs to this constructor.
    pub fn into_literal_value(self, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
        assert!(
            matches!(&*self, ScalarExpr::Literal(_)),
            "literal value extraction requires an untyped literal"
        );
        let (expression, memory) = self.finish(control)?.into_parts();
        let ScalarExpr::Literal(value) = expression else {
            panic!("literal value extraction requires an untyped literal");
        };
        Ok(control.finish(value, memory)?)
    }

    /// Replace a literal's declared type and collation while preserving its parameter and original composite source. The old leaf's allocating fields share one constructor lease, so the replacement copies surviving fields under their producers before releasing that complete old lease.
    pub fn replace_literal_type(
        self,
        declaration: Produced<String>,
        bound_type: Produced<ColumnType>,
        collation_oid: Option<u32>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        control.check()?;
        let (value, composite_source, parameter_index) = match &self.expression {
            ScalarExpr::Literal(value) => (value, None, None),
            ScalarExpr::TypedLiteral {
                value,
                composite_source,
                parameter_index,
                ..
            } => (value, composite_source.as_ref(), *parameter_index),
            _ => panic!("literal type replacement requires an inline literal"),
        };
        let mut lowering = Lowering {
            control: control.budget().map(|_| Control::from_production(*control)),
            owned_children: control.budget().map(BudgetedVec::new),
        };
        let composite_source = lowering.composite_source(composite_source.map(Source::Borrowed))?;
        let value = lowering.value(Source::Borrowed(value))?;
        let (ty, declaration_memory) = declaration.into_parts();
        let (bound_type, type_memory) = bound_type.into_parts();
        let metadata = control.combine(declaration_memory, type_memory);
        if let Some(owner) = &mut lowering.control {
            owner
                .memory
                .absorb(metadata.expect("admitted literal metadata"));
        }
        let output = Self {
            expression: ScalarExpr::TypedLiteral {
                value,
                ty,
                bound_type: Some(bound_type),
                parameter_index,
                composite_source,
                collation_oid,
            },
            memory: lowering.control.map(|control| ScalarMemory {
                direct: control.memory,
                children: lowering
                    .owned_children
                    .expect("mutable literal construction"),
            }),
        };
        drop(self);
        control.check()?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn literal_value_transfer_preserves_payload_identity_and_complete_lease() {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let source = Value::Str("payload".repeat(1024));
        let value = control.copy_value(&source).unwrap();
        let Value::Str(text) = &*value else {
            unreachable!()
        };
        let pointer = text.as_ptr();
        let before = budget.used();
        let mut output = None;
        let allocations = allocation_counter::measure(|| {
            let expression = OwnedScalarExpr::literal(value, &control).unwrap();
            output = Some(expression.into_literal_value(&control).unwrap());
        });
        let output = output.unwrap();
        let Value::Str(text) = &*output else {
            unreachable!()
        };
        assert_eq!(text.as_ptr(), pointer);
        assert_eq!(allocations.count_total, 0);
        assert_eq!(output.reserved_bytes(), before);
        assert_eq!(budget.used(), before);
        drop(output);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn literal_type_replacement_preserves_original_tuple_and_parameter_identity() {
        let source = ScalarExpr::TypedLiteral {
            value: Value::Str("current".repeat(1024)),
            ty: "text".into(),
            bound_type: Some(ColumnType::Text),
            parameter_index: Some(7),
            collation_oid: Some(100),
            composite_source: Some(Box::new(crate::expr::composites::CompositeConstantSource {
                value: Value::Str("original".repeat(1024)),
                descriptors: Vec::new(),
            })),
        };
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut output = None;
        let allocations = allocation_counter::measure(|| {
            let input = source.copy_for_rewrite(&control).unwrap();
            output = Some(
                input
                    .replace_literal_type(
                        control.copy_text("pg_catalog.varchar(10)").unwrap(),
                        ColumnType::Varchar(Some(10))
                            .clone_with_control(&control)
                            .unwrap(),
                        Some(951),
                        &control,
                    )
                    .unwrap(),
            );
        });
        let output = output.unwrap();
        let ScalarExpr::TypedLiteral {
            value,
            ty,
            bound_type,
            parameter_index,
            collation_oid,
            composite_source,
        } = &*output
        else {
            panic!("typed literal")
        };
        assert_eq!(value, &Value::Str("current".repeat(1024)));
        assert_eq!(ty, "pg_catalog.varchar(10)");
        assert_eq!(bound_type, &Some(ColumnType::Varchar(Some(10))));
        assert_eq!(*parameter_index, Some(7));
        assert_eq!(*collation_oid, Some(951));
        assert_eq!(
            composite_source.as_ref().unwrap().value,
            Value::Str("original".repeat(1024))
        );
        assert_eq!(
            allocations.bytes_current,
            i64::try_from(budget.used()).unwrap()
        );
        assert_eq!(output.reserved_bytes(), budget.used());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}
