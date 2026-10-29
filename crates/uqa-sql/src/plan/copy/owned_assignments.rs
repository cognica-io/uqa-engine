//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, RewriteMemory};
use crate::{
    ast::AssignmentTargets,
    plan::{
        mutable::commands::rule_inputs::{OwnedAssignment, OwnedTarget},
        AssignmentPlan, ScalarExpr,
    },
};
use uqa_core::{memory::ProductionControl, Value};

impl Copying<'_> {
    fn adopt_fields(&mut self, memory: RewriteMemory) {
        let RewriteMemory::Fields { direct, fields } = memory else {
            unreachable!("structural constructor")
        };
        assert!(
            self.control
                .budget()
                .is_some_and(|budget| budget.shares_allowance(direct.budget())),
            "assignment constructor allowance"
        );
        assert_eq!(
            self.memory.bytes(),
            0,
            "destination constructor has no prior allocations"
        );
        assert!(
            self.fields.as_ref().is_some_and(|fields| fields.is_empty()),
            "destination constructor has no prior fields"
        );
        self.memory = direct;
        self.fields = Some(fields);
    }
    fn take_assignment(&mut self, assignment: OwnedAssignment) -> Result<AssignmentPlan> {
        self.check()?;
        self.adopt_fields(assignment.memory.expect("admitted assignment"));
        Ok(assignment.value)
    }
}

pub(crate) fn null_assignment(
    target: OwnedTarget,
    control: &ProductionControl<'_>,
) -> Result<OwnedAssignment> {
    control.check()?;
    let Some(mut copying) = Copying::for_rewrite(*control) else {
        return Ok(OwnedAssignment {
            value: AssignmentPlan {
                target: AssignmentTargets::Single(target.value),
                value: ScalarExpr::Literal(Value::Null),
            },
            memory: None,
        });
    };
    let target = copying.field("target", |copying| {
        copying.adopt_fields(target.memory.expect("admitted assignment target"));
        Ok(AssignmentTargets::Single(target.value))
    })?;
    let value = copying.field("value", |copying| {
        copying.scalar(&ScalarExpr::Literal(Value::Null))
    })?;
    Ok(OwnedAssignment {
        value: AssignmentPlan { target, value },
        memory: Some(copying.into_rewrite_memory()),
    })
}

pub(crate) fn assignment_vector(
    values: impl ExactSizeIterator<Item = OwnedAssignment>,
    control: &ProductionControl<'_>,
) -> Result<(Vec<AssignmentPlan>, Option<RewriteMemory>)> {
    control.check()?;
    let Some(mut copying) = Copying::for_rewrite(*control) else {
        return Ok((values.map(|assignment| assignment.value).collect(), None));
    };
    let values = copying.map(values, Copying::take_assignment)?;
    Ok((values, Some(copying.into_rewrite_memory())))
}
