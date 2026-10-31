//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dependency selection admits each distinct identity before allocating its ordered set node.

use super::PreparedAnalysisDependencies;
use crate::{ast::FunctionBinding, ColumnType, SQLError, ScalarExpr};
use uqa_core::{memory::ProductionControl, Value};

impl PreparedAnalysisDependencies {
    pub(crate) fn include_relation_with_control(
        &mut self,
        oid: u32,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        match control.budget() {
            Some(budget) => {
                self.relations.insert_budgeted(oid, budget)?;
            }
            None => {
                self.relations.insert(oid);
            }
        }
        Ok(())
    }

    pub(crate) fn include_routine_with_control(
        &mut self,
        binding: &FunctionBinding,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        if !binding.builtin {
            if let Some(identity) = binding.object_id {
                self.include_routine_identity_with_control(identity, control)?;
            }
        }
        Ok(())
    }

    pub(crate) fn include_routine_identity_with_control(
        &mut self,
        identity: [u8; 16],
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        match control.budget() {
            Some(budget) => {
                self.routines.insert_budgeted(identity, budget)?;
            }
            None => {
                self.routines.insert(identity);
            }
        }
        Ok(())
    }

    pub(crate) fn include_expression_with_control(
        &mut self,
        expression: &ScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        expression.try_visit(&mut |expression| {
            control.check()?;
            match expression {
                ScalarExpr::TypedLiteral {
                    value: Value::Int(oid),
                    ty,
                    bound_type,
                    parameter_index: None,
                    ..
                } => {
                    let parsed;
                    let ty = if let Some(ty) = bound_type {
                        Some(ty)
                    } else {
                        parsed = match ColumnType::from_sql_name_with_control(ty, control) {
                            Ok(ty) => Some(ty),
                            Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                                return Err(error)
                            }
                            Err(_) => None,
                        };
                        parsed.as_deref()
                    };
                    if matches!(ty, Some(ColumnType::Regclass | ColumnType::Oid)) {
                        if let Ok(oid) = u32::try_from(*oid) {
                            self.include_relation_with_control(oid, control)?;
                        }
                    }
                }
                ScalarExpr::Func {
                    binding: Some(binding),
                    ..
                } => self.include_routine_with_control(binding, control)?,
                _ => {}
            }
            Ok::<_, SQLError>(true)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn dependency_nodes_keep_exact_lease_until_last_identity_is_retired() {
        let budget = MemoryBudget::new(4096);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut output = PreparedAnalysisDependencies::default();
        let allocations = allocation_counter::measure(|| {
            output.include_relation_with_control(17, &control).unwrap();
            output
                .include_routine_identity_with_control([3; 16], &control)
                .unwrap();
        });
        assert_eq!(allocations.bytes_current as usize, budget.used());
        assert_eq!(allocations.count_current, 2);
        let retained = budget.used();
        let duplicate = allocation_counter::measure(|| {
            output.include_relation_with_control(17, &control).unwrap();
            output
                .include_routine_identity_with_control([3; 16], &control)
                .unwrap();
        });
        assert_eq!(duplicate.count_total, 0);
        assert_eq!(budget.used(), retained);
        assert!(output.relations.remove(&17));
        assert_eq!(budget.used(), output.routines.allocated_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn dependency_admission_rejection_and_both_cancellations_leave_no_nodes() {
        for mode in 0..3 {
            let budget = MemoryBudget::new(if mode == 0 { 0 } else { 4096 });
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&budget, &original, &invoking);
            match mode {
                1 => original.cancel(),
                2 => invoking.cancel(),
                _ => {}
            }
            // Initialize the platform's cancelled-token lock outside producer allocation measurement.
            if mode != 0 {
                assert!(control.check().is_err());
            }
            let diagnostic = allocation_counter::measure(|| {
                if mode == 0 {
                    drop(SQLError::from(budget.reserve(1).unwrap_err()));
                } else {
                    drop(SQLError::from(control.check().unwrap_err()));
                }
            });
            let mut output = PreparedAnalysisDependencies::default();
            let allocation = allocation_counter::measure(|| {
                let error = output
                    .include_relation_with_control(17, &control)
                    .unwrap_err();
                assert_eq!(
                    error.sqlstate(),
                    Some(if mode == 0 { "53200" } else { "57014" })
                );
            });
            assert_eq!(allocation.count_total, diagnostic.count_total);
            assert!(allocation.bytes_total < 512);
            assert!(output.relations.is_empty());
            assert_eq!(budget.used(), 0);
        }
    }
}
