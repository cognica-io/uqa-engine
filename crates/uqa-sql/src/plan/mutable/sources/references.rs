//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical relation names replace only their independently owned source metadata.

use super::MutableSourcePlan;
use crate::plan::mutable::memory::RewriteMemory;
use crate::{
    ast::OperatorJoinRelations,
    plan::{MutableQueryPlan, SourcePlan},
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

impl MutableSourcePlan {
    /// Replace an explicit source alias without changing the source's physical or function identity.
    pub fn replace_alias(
        mut self,
        alias: Produced<String>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let current = match &mut self.source {
            SourcePlan::Table { alias, .. }
            | SourcePlan::Join { alias, .. }
            | SourcePlan::Values { alias, .. }
            | SourcePlan::Subquery { alias, .. }
            | SourcePlan::Function { alias, .. }
            | SourcePlan::FunctionGroup { alias, .. } => alias,
            SourcePlan::RetainedRows(_) => {
                return Err(SQLError::Internal(
                    "retained rows have no source alias".into(),
                ));
            }
        };
        let (alias, lease) = alias.into_parts();
        let (alias, lease) = control.finish(alias, lease)?.into_parts();
        let previous_capacity = current.as_ref().map_or(0, String::capacity);
        *current = Some(alias);
        if let Some(memory) = self.memory.as_mut() {
            let RewriteMemory::Fields { direct, .. } = memory else {
                unreachable!("source constructor")
            };
            // Source constructors retain the alias buffer directly beside their metadata.
            drop(direct.split(previous_capacity));
            direct.absorb(lease.expect("controlled source alias"));
        }
        control.check()?;
        Ok(self)
    }

    pub fn replace_table_reference(
        mut self,
        name: Option<Produced<String>>,
        qualifier: Option<Produced<String>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.replace_table_reference_in_place(name, qualifier, control)?;
        Ok(self)
    }

    pub(crate) fn replace_table_reference_in_place(
        &mut self,
        name: Option<Produced<String>>,
        qualifier: Option<Produced<String>>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.assert_owner(control);
        control.check()?;
        let SourcePlan::Table {
            name: current,
            qualifier: current_qualifier,
            ..
        } = &mut self.source
        else {
            return Err(SQLError::Internal(
                "relation-name replacement requires a table source".into(),
            ));
        };
        if let Some(name) = name {
            install(current, self.memory.as_mut(), "name", name, control)?;
        }
        if let Some(qualifier) = qualifier {
            install(
                current_qualifier,
                self.memory.as_mut(),
                "qualifier",
                qualifier,
                control,
            )?;
        }
        Ok(())
    }

    pub fn replace_function_references(
        mut self,
        index: usize,
        output_name: Option<Produced<String>>,
        relations: Option<Produced<OperatorJoinRelations>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.replace_function_references_in_place(index, output_name, relations, control)?;
        Ok(self)
    }

    pub(crate) fn replace_function_references_in_place(
        &mut self,
        index: usize,
        output_name: Option<Produced<String>>,
        relations: Option<Produced<OperatorJoinRelations>>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.assert_owner(control);
        control.check()?;
        let (name, current_relations, mut memory) = match &mut self.source {
            SourcePlan::Function {
                output_name,
                relations,
                ..
            } if index == 0 => (output_name, relations, self.memory.as_mut()),
            SourcePlan::FunctionGroup { functions, .. } => {
                let function = &mut functions[index];
                let memory = self.memory.as_mut().map(|memory| {
                    memory
                        .field_mut("functions")
                        .as_mut()
                        .expect("function group owner")
                        .item_mut(index)
                        .as_mut()
                        .expect("function member owner")
                });
                (&mut function.output_name, &mut function.relations, memory)
            }
            _ => {
                return Err(SQLError::Internal(
                    "function-name replacement requires a function source".into(),
                ))
            }
        };
        if let Some(output_name) = output_name {
            install(
                name,
                memory.as_deref_mut(),
                "output_name",
                output_name,
                control,
            )?;
        }
        if let Some(relations) = relations {
            let (relations, lease) = relations.into_parts();
            install(
                current_relations,
                memory,
                "relations",
                control.finish(Some(relations), lease)?,
                control,
            )?;
        }
        Ok(())
    }
}

fn install<T>(
    target: &mut T,
    memory: Option<&mut RewriteMemory>,
    field: &'static str,
    value: Produced<T>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let (value, lease) = value.into_parts();
    let (value, lease) = control.finish(value, lease)?.into_parts();
    *target = value;
    if let Some(memory) = memory {
        let direct = lease.expect("controlled source metadata");
        *memory.field_mut(field) = Some(RewriteMemory::direct(direct));
    }
    Ok(())
}

impl MutableQueryPlan {
    pub fn set_relations_bound(
        mut self,
        bound: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.set_relations_bound_in_place(bound, control)?;
        Ok(self)
    }

    pub(crate) fn set_relations_bound_in_place(
        &mut self,
        bound: bool,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.assert_owner(control);
        control.check()?;
        self.query.relations_bound = bound;
        Ok(())
    }
}
