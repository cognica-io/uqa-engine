//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FROM routine bindings transfer independently from their source arguments and range metadata.

use super::super::super::copy::Copying;
use super::super::memory::RewriteMemory;
use super::{MutableSourcePlan, SourcePlan};
use crate::{ast::FunctionBinding, plan::TableFunctionPlan, SQLError};
use uqa_core::memory::{Produced, ProductionControl};

impl TableFunctionPlan {
    /// Build the temporary member used by execution binding, without inheriting the surrounding group's alias or ordinality.
    pub(crate) fn copy_for_execution(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<MutableSourcePlan, SQLError> {
        control.check()?;
        if let Some(mut copying) = Copying::for_rewrite(*control) {
            let source = copying.function_group_member(self, None)?;
            return Ok(MutableSourcePlan {
                source,
                memory: Some(copying.into_rewrite_memory()),
            });
        }
        let function = self.clone();
        Ok(MutableSourcePlan {
            source: SourcePlan::Function {
                name: function.name,
                binding: function.binding,
                output_name: function.output_name,
                relations: function.relations,
                args: function.args,
                alias: None,
                column_aliases: function.column_aliases,
                column_types: function.column_types,
                ordinality: false,
            },
            memory: None,
        })
    }
}

impl MutableSourcePlan {
    pub(crate) fn replace_function_binding(
        mut self,
        member: Option<usize>,
        binding: Produced<Option<FunctionBinding>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let (binding, memory) = if let Some(mut copying) = Copying::for_rewrite(*control) {
            let binding = copying.take(binding);
            (binding, Some(copying.into_rewrite_memory()))
        } else {
            (
                binding
                    .into_uncontrolled()
                    .expect("ordinary function binding"),
                None,
            )
        };
        let destination = match (&mut self.source, member) {
            (SourcePlan::Function { binding, .. }, None) => binding,
            (SourcePlan::FunctionGroup { functions, .. }, Some(index)) => {
                &mut functions[index].binding
            }
            _ => unreachable!("selected function binding slot"),
        };
        *destination = binding;
        if let Some(owner) = &mut self.memory {
            *binding_memory(owner, member) = memory;
        }
        control.check()?;
        Ok(self)
    }

    /// Retire the private member and its arguments after transferring only its selected function identity.
    pub(crate) fn into_function_binding(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Option<FunctionBinding>>, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let SourcePlan::Function { binding, .. } = &mut self.source else {
            unreachable!("function member")
        };
        let binding = binding.take();
        let memory = self
            .memory
            .as_mut()
            .and_then(|owner| binding_memory(owner, None).take())
            .map(RewriteMemory::into_reservation);
        drop(self);
        Ok(control.finish(binding, memory)?)
    }
}

fn binding_memory(memory: &mut RewriteMemory, member: Option<usize>) -> &mut Option<RewriteMemory> {
    let memory = match member {
        Some(index) => super::item(memory, "functions", index),
        None => memory,
    };
    memory.field_mut("binding")
}
