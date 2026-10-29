//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Call metadata replacement transfers children while old constructor storage remains owned.

use super::{OwnedScalarExpr, Result, ScalarExpr, ScalarMemory};
use crate::{
    ast::FunctionBinding,
    plan::scalar::resources::{Control, Lowering},
    ScalarOrder,
};
use uqa_core::memory::{BudgetedVec, Produced, ProductionControl};

impl OwnedScalarExpr {
    /// Preserve the parser's inline ordering-syntax identity without rebuilding any call storage.
    pub fn set_call_order_syntax(
        mut self,
        syntax: crate::ast::FunctionOrderSyntax,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        control.check()?;
        let ScalarExpr::Func { order_syntax, .. } = &mut self.expression else {
            panic!("ordering syntax requires a function call")
        };
        *order_syntax = syntax;
        Ok(self)
    }

    /// Change an inline call dispatch without moving or replacing any allocating field.
    pub fn set_call_dispatch(
        mut self,
        dispatch: Option<crate::ast::FunctionDispatch>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        control.check()?;
        let ScalarExpr::Func {
            binding: Some(binding),
            ..
        } = &mut self.expression
        else {
            panic!("dispatch mutation requires a bound function call")
        };
        binding.dispatch = dispatch;
        Ok(self)
    }

    /// Replace a call's selected binding without changing its name, arguments, ordering or filter. The old metadata and containers remain owned until the replacement's admitted containers hold all surviving children.
    pub fn replace_call_binding(
        self,
        binding: Option<Produced<FunctionBinding>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        let ScalarExpr::Func { name, .. } = &self.expression else {
            panic!("binding replacement requires a function call");
        };
        let name = control.copy_text(name)?;
        self.replace_call_metadata(name, binding, control)
    }

    /// Install an independently produced call name and binding. This storage operation retains scalar payload identity and performs no overload selection or expression evaluation.
    pub fn replace_call_metadata(
        mut self,
        name: Produced<String>,
        binding: Option<Produced<FunctionBinding>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        control.check()?;
        let ScalarExpr::Func {
            args,
            order_by,
            filter,
            distinct,
            order_syntax,
            ..
        } = &self.expression
        else {
            panic!("metadata replacement requires a function call");
        };
        let (argument_count, order_count, has_filter, distinct, order_syntax) = (
            args.len(),
            order_by.len(),
            filter.is_some(),
            *distinct,
            *order_syntax,
        );
        let mut lowering = Lowering {
            control: control.budget().map(|_| Control::from_production(*control)),
            owned_children: control.budget().map(BudgetedVec::new),
        };
        let (name, name_memory) = name.into_parts();
        let (binding, binding_memory) = match binding {
            Some(binding) => {
                let (binding, memory) = binding.into_parts();
                (Some(binding), memory)
            }
            None => (None, control.empty_reservation()),
        };
        let metadata = control.combine(name_memory, binding_memory);
        if let Some(owner) = &mut lowering.control {
            owner
                .memory
                .absorb(metadata.expect("admitted call metadata"));
        }
        let args = lowering.map(0..argument_count, |lowering, index| {
            Self::attach_child(lowering, self.take_child(index))
        })?;
        let order_by = lowering.map(0..order_count, |lowering, index| {
            let ScalarExpr::Func { order_by, .. } = &self.expression else {
                unreachable!();
            };
            let (descending, nulls) = (order_by[index].descending, order_by[index].nulls);
            Ok(ScalarOrder {
                expr: Self::attach_child(lowering, self.take_child(argument_count + index))?,
                descending,
                nulls,
            })
        })?;
        let filter = has_filter
            .then(|| {
                lowering.boxed(|lowering| {
                    Self::attach_child(lowering, self.take_child(argument_count + order_count))
                })
            })
            .transpose()?;
        let output = Self {
            expression: ScalarExpr::Func {
                order_syntax,
                name,
                binding,
                args,
                distinct,
                order_by,
                filter,
            },
            memory: lowering.control.map(|control| ScalarMemory {
                direct: control.memory,
                children: lowering.owned_children.expect("mutable call construction"),
            }),
        };
        drop(self);
        control.check()?;
        Ok(output)
    }
}
