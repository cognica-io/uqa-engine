//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Replacement nodes keep their constructor inputs and independently movable children owned.

use super::{OwnedScalarExpr, Result, ScalarExpr, ScalarMemory};
use crate::{ir::RetainedScalarLiteral, ColumnType};
use uqa_core::{
    memory::{BudgetedVec, Produced, ProductionControl},
    Value,
};

impl OwnedScalarExpr {
    /// Construct a typed literal from independently admitted value and type metadata. This operation only transfers storage; it performs no type resolution, conversion or evaluation.
    pub fn typed_literal(
        value: Produced<Value>,
        declaration: Produced<String>,
        bound_type: Option<Produced<ColumnType>>,
        collation_oid: Option<u32>,
        parameter_index: Option<usize>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        control.check()?;
        let mut output = Self {
            expression: ScalarExpr::Literal(Value::Null),
            memory: control.empty_reservation().map(|direct| ScalarMemory {
                children: BudgetedVec::new(direct.budget()),
                direct,
            }),
        };
        let (value, value_memory) = value.into_parts();
        let (ty, type_memory) = declaration.into_parts();
        let (bound_type, bound_memory) = match bound_type {
            Some(ty) => {
                let (ty, memory) = ty.into_parts();
                (Some(ty), memory)
            }
            None => (None, control.empty_reservation()),
        };
        output.expression = ScalarExpr::TypedLiteral {
            value,
            ty,
            bound_type,
            collation_oid,
            composite_source: None,
            parameter_index,
        };
        let memory = control.combine(control.combine(value_memory, type_memory), bound_memory);
        if let Some(owner) = &mut output.memory {
            owner
                .direct
                .absorb(memory.expect("admitted literal fields"));
        }
        control.check()?;
        Ok(output)
    }

    /// Attach an already retained constant without charging its independently shared payload a second time.
    pub fn retained_literal(
        literal: RetainedScalarLiteral,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        let output = Self {
            expression: ScalarExpr::RetainedLiteral(literal),
            memory: control.empty_reservation().map(|direct| ScalarMemory {
                children: BudgetedVec::new(direct.budget()),
                direct,
            }),
        };
        control.check()?;
        Ok(output)
    }

    /// Wrap an independently owned child in a cast. The child box and ownership slot are admitted before allocation; the destination declaration retains its original producer lease.
    pub fn cast(
        child: Self,
        declaration: Produced<String>,
        implicit: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        child.assert_owner(control);
        control.check()?;
        let mut output = Self {
            expression: ScalarExpr::Literal(Value::Null),
            memory: control.empty_reservation().map(|direct| ScalarMemory {
                children: BudgetedVec::new(direct.budget()),
                direct,
            }),
        };
        if let Some(owner) = &mut output.memory {
            owner.children.reserve(1)?;
        }
        let boxed = control.reserve(size_of::<ScalarExpr>())?;
        let Self { expression, memory } = child;
        let (ty, declaration_memory) = declaration.into_parts();
        output.expression = ScalarExpr::Cast {
            expr: Box::new(expression),
            ty,
            implicit,
        };
        let direct = control.combine(boxed, declaration_memory);
        if let Some(owner) = &mut output.memory {
            owner
                .children
                .push(memory)
                .expect("admitted cast child slot");
            owner.direct.absorb(direct.expect("admitted cast fields"));
        }
        control.check()?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn typed_replacements_and_casts_preserve_exact_live_constructor_ownership() {
        let value = Value::Str("payload".repeat(1024));
        let ty = ColumnType::Named("catalog.type".repeat(32));
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut output = None;
        let allocated = allocation_counter::measure(|| {
            let child = OwnedScalarExpr::typed_literal(
                control.copy_value(&value).unwrap(),
                control.copy_text("declared_type").unwrap(),
                Some(ty.clone_with_control(&control).unwrap()),
                Some(101),
                Some(3),
                &control,
            )
            .unwrap();
            output = Some(
                OwnedScalarExpr::cast(
                    child,
                    control.copy_text("converted_type").unwrap(),
                    true,
                    &control,
                )
                .unwrap(),
            );
        });
        let output = output.unwrap();
        assert_eq!(
            allocated.bytes_current,
            i64::try_from(budget.used()).unwrap()
        );
        assert_eq!(output.reserved_bytes(), budget.used());
        let ScalarExpr::Cast {
            expr,
            ty: name,
            implicit,
        } = &*output
        else {
            panic!("owned cast");
        };
        assert_eq!(name, "converted_type");
        assert!(*implicit);
        let ScalarExpr::TypedLiteral {
            value: Value::Str(payload),
            ..
        } = expr.as_ref()
        else {
            panic!("owned typed child");
        };
        let pointer = payload.as_ptr();
        let before = budget.used();
        let mut selected = None;
        let released = allocation_counter::measure(|| selected = Some(output.select_child(0)));
        let selected = selected.unwrap();
        assert_eq!(released.count_total, 0);
        assert_eq!(
            released.bytes_current,
            -(i64::try_from(before).unwrap() - i64::try_from(budget.used()).unwrap())
        );
        let ScalarExpr::TypedLiteral {
            value: Value::Str(payload),
            bound_type,
            collation_oid,
            parameter_index,
            ..
        } = &*selected
        else {
            panic!("selected typed child");
        };
        assert_eq!(payload.as_ptr(), pointer);
        assert_eq!(bound_type.as_ref(), Some(&ty));
        assert_eq!(*collation_oid, Some(101));
        assert_eq!(*parameter_index, Some(3));
        drop(selected);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn cast_parent_admission_and_cancellation_destroy_both_private_inputs() {
        for mode in 0..4 {
            let budget = MemoryBudget::new(1 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&budget, &original, &invoking);
            let child = ScalarExpr::Literal(Value::Str("x".repeat(1024)))
                .copy_for_rewrite(&control)
                .unwrap();
            let name = control.copy_text("integer").unwrap();
            let spare = if mode == 1 {
                size_of::<Option<ScalarMemory>>()
            } else {
                0
            };
            let sibling = budget.reserve(budget.available() - spare).unwrap();
            if mode == 2 {
                original.cancel();
            }
            if mode == 3 {
                invoking.cancel();
            }
            let error: crate::SQLError = OwnedScalarExpr::cast(child, name, false, &control)
                .unwrap_err()
                .into();
            assert_eq!(
                error.sqlstate(),
                Some(if mode < 2 { "53200" } else { "57014" })
            );
            assert_eq!(budget.used(), sibling.bytes());
            drop(sibling);
            assert_eq!(budget.used(), 0);
        }
    }

    #[test]
    fn retained_leaf_does_not_double_charge_its_shared_constant() {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let value = retained_value(&control);
        let literal =
            RetainedScalarLiteral::with_control(value, None, None, None, &control).unwrap();
        let last = literal.clone();
        let before = budget.used();
        let mut output = None;
        let allocated = allocation_counter::measure(|| {
            output = Some(OwnedScalarExpr::retained_literal(literal, &control).unwrap());
        });
        let output = output.unwrap();
        assert_eq!(allocated.count_total, 0);
        assert_eq!(output.reserved_bytes(), 0);
        assert_eq!(budget.used(), before);
        drop(output);
        assert_eq!(budget.used(), before);
        drop(last);
        assert_eq!(budget.used(), 0);
    }

    fn retained_value(control: &ProductionControl<'_>) -> crate::ir::RetainedConstantValue {
        crate::ir::RetainedConstantValue::from_produced(
            control.copy_value(&Value::Str("x".repeat(1024))).unwrap(),
            control,
        )
        .unwrap()
    }
}
