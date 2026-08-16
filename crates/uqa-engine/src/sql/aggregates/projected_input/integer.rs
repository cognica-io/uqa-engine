//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional integer expression bytecode for analytical aggregates.

use uqa_core::Value;
use uqa_execution::{RowSchema, ScalarExpr};
use uqa_sql::ast::BinaryOp;
use uqa_sql::expr::RowLookup;
use uqa_sql::SQLError;

use super::column_slot;

const INTEGER_STACK_LIMIT: usize = 16;

#[derive(Clone, Copy)]
pub(super) enum ProjectedIntegerValue {
    Integer(i64),
    Null,
    General,
}

#[derive(Clone, Copy)]
enum ProjectedIntegerInstruction {
    Slot(usize),
    Literal(Option<i64>),
    Binary(BinaryOp),
}

#[derive(Clone, Copy)]
enum ProjectedIntegerOperand {
    Slot(usize),
    Literal(Option<i64>),
}

enum ProjectedIntegerPlan {
    DirectBinary {
        operator: BinaryOp,
        left: ProjectedIntegerOperand,
        right: ProjectedIntegerOperand,
    },
    Program(Vec<ProjectedIntegerInstruction>),
}

pub(super) struct ProjectedIntegerExpression {
    plan: ProjectedIntegerPlan,
}

impl ProjectedIntegerExpression {
    pub(super) fn compile(expression: &ScalarExpr, input_schema: &RowSchema) -> Option<Self> {
        if let ScalarExpr::Binary { op, lhs, rhs } = expression {
            if matches!(
                op,
                BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
            ) {
                if let (Some(left), Some(right)) = (
                    compile_integer_operand(lhs, input_schema),
                    compile_integer_operand(rhs, input_schema),
                ) {
                    return Some(Self {
                        plan: ProjectedIntegerPlan::DirectBinary {
                            operator: *op,
                            left,
                            right,
                        },
                    });
                }
            }
        }
        let mut instructions = Vec::new();
        let mut stack_depth = 0usize;
        let mut max_stack_depth = 0usize;
        emit_integer_expression(
            expression,
            input_schema,
            &mut instructions,
            &mut stack_depth,
            &mut max_stack_depth,
        )?;
        (stack_depth == 1 && max_stack_depth <= INTEGER_STACK_LIMIT).then_some(Self {
            plan: ProjectedIntegerPlan::Program(instructions),
        })
    }

    pub(super) fn evaluate<Row: RowLookup>(
        &self,
        row: &Row,
    ) -> Result<ProjectedIntegerValue, SQLError> {
        let instructions = match &self.plan {
            ProjectedIntegerPlan::DirectBinary {
                operator,
                left,
                right,
            } => {
                return evaluate_integer_binary(
                    *operator,
                    evaluate_integer_operand(*left, row),
                    evaluate_integer_operand(*right, row),
                )
            }
            ProjectedIntegerPlan::Program(instructions) => instructions,
        };
        let mut stack = [ProjectedIntegerValue::General; INTEGER_STACK_LIMIT];
        let mut stack_len = 0usize;
        for instruction in instructions {
            match *instruction {
                ProjectedIntegerInstruction::Slot(slot) => {
                    stack[stack_len] = match row.positional_column(slot) {
                        Some(Value::Int(value)) => ProjectedIntegerValue::Integer(*value),
                        Some(Value::Null) | None => ProjectedIntegerValue::Null,
                        Some(_) => return Ok(ProjectedIntegerValue::General),
                    };
                    stack_len += 1;
                }
                ProjectedIntegerInstruction::Literal(value) => {
                    stack[stack_len] =
                        value.map_or(ProjectedIntegerValue::Null, ProjectedIntegerValue::Integer);
                    stack_len += 1;
                }
                ProjectedIntegerInstruction::Binary(operator) => {
                    debug_assert!(stack_len >= 2);
                    let right = stack[stack_len - 1];
                    let left = stack[stack_len - 2];
                    stack_len -= 1;
                    stack[stack_len - 1] = evaluate_integer_binary(operator, left, right)?;
                }
            }
        }
        debug_assert_eq!(stack_len, 1);
        Ok(stack[0])
    }
}

fn compile_integer_operand(
    expression: &ScalarExpr,
    input_schema: &RowSchema,
) -> Option<ProjectedIntegerOperand> {
    match expression {
        ScalarExpr::Column(_) | ScalarExpr::QualifiedColumn { .. } => Some(
            ProjectedIntegerOperand::Slot(column_slot(expression, input_schema)?),
        ),
        ScalarExpr::Position(position) if *position < input_schema.len() => {
            Some(ProjectedIntegerOperand::Slot(*position))
        }
        ScalarExpr::Literal(Value::Int(value)) => {
            Some(ProjectedIntegerOperand::Literal(Some(*value)))
        }
        ScalarExpr::Literal(Value::Null) => Some(ProjectedIntegerOperand::Literal(None)),
        _ => None,
    }
}

fn evaluate_integer_operand<Row: RowLookup>(
    operand: ProjectedIntegerOperand,
    row: &Row,
) -> ProjectedIntegerValue {
    match operand {
        ProjectedIntegerOperand::Slot(slot) => match row.positional_column(slot) {
            Some(Value::Int(value)) => ProjectedIntegerValue::Integer(*value),
            Some(Value::Null) | None => ProjectedIntegerValue::Null,
            Some(_) => ProjectedIntegerValue::General,
        },
        ProjectedIntegerOperand::Literal(value) => {
            value.map_or(ProjectedIntegerValue::Null, ProjectedIntegerValue::Integer)
        }
    }
}

fn emit_integer_expression(
    expression: &ScalarExpr,
    input_schema: &RowSchema,
    instructions: &mut Vec<ProjectedIntegerInstruction>,
    stack_depth: &mut usize,
    max_stack_depth: &mut usize,
) -> Option<()> {
    match expression {
        ScalarExpr::Column(_) | ScalarExpr::QualifiedColumn { .. } => {
            instructions.push(ProjectedIntegerInstruction::Slot(column_slot(
                expression,
                input_schema,
            )?));
            *stack_depth += 1;
        }
        ScalarExpr::Literal(Value::Int(value)) => {
            instructions.push(ProjectedIntegerInstruction::Literal(Some(*value)));
            *stack_depth += 1;
        }
        ScalarExpr::Literal(Value::Null) => {
            instructions.push(ProjectedIntegerInstruction::Literal(None));
            *stack_depth += 1;
        }
        ScalarExpr::Binary { op, lhs, rhs }
            if matches!(
                op,
                BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
            ) =>
        {
            emit_integer_expression(
                lhs,
                input_schema,
                instructions,
                stack_depth,
                max_stack_depth,
            )?;
            emit_integer_expression(
                rhs,
                input_schema,
                instructions,
                stack_depth,
                max_stack_depth,
            )?;
            instructions.push(ProjectedIntegerInstruction::Binary(*op));
            *stack_depth = stack_depth.checked_sub(1)?;
        }
        _ => return None,
    }
    *max_stack_depth = (*max_stack_depth).max(*stack_depth);
    Some(())
}

fn evaluate_integer_binary(
    operator: BinaryOp,
    left: ProjectedIntegerValue,
    right: ProjectedIntegerValue,
) -> Result<ProjectedIntegerValue, SQLError> {
    match (left, right) {
        (ProjectedIntegerValue::Integer(left), ProjectedIntegerValue::Integer(right)) => {
            let value = match operator {
                BinaryOp::Add => left.checked_add(right),
                BinaryOp::Subtract => left.checked_sub(right),
                BinaryOp::Multiply => left.checked_mul(right),
                BinaryOp::Divide if right != 0 => left.checked_div(right),
                BinaryOp::Divide => None,
                _ => unreachable!("compiled integer aggregate operator"),
            };
            if let Some(value) = value {
                return Ok(ProjectedIntegerValue::Integer(value));
            }
            let value =
                uqa_sql::expr::eval_binary_values(operator, &Value::Int(left), &Value::Int(right))?;
            Ok(match value {
                Value::Int(value) => ProjectedIntegerValue::Integer(value),
                _ => ProjectedIntegerValue::General,
            })
        }
        (ProjectedIntegerValue::Null, ProjectedIntegerValue::Null)
        | (ProjectedIntegerValue::Null, ProjectedIntegerValue::Integer(_))
        | (ProjectedIntegerValue::Integer(_), ProjectedIntegerValue::Null) => {
            Ok(ProjectedIntegerValue::Null)
        }
        _ => Ok(ProjectedIntegerValue::General),
    }
}
