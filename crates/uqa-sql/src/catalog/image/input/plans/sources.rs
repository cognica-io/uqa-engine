//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::scalars::References;
use super::{
    driver::Step, empty_query, empty_source, invalid, Input, Kind, MemoryReservation, Node,
    SQLError,
};
use crate::{
    ast::{InternalRelationId, JoinKind},
    plan::{JoinExecutionStrategy, QueryPlan, SourcePlan},
};

#[expect(
    clippy::too_many_lines,
    reason = "native source field order and admitted child boundaries"
)]
pub(super) fn read(
    value: &mut SourcePlan,
    position: usize,
    references: &mut References,
    input: &mut Input<'_>,
    memory: &mut MemoryReservation,
) -> Result<Step, SQLError> {
    if position == 0 {
        *value = node(input, memory)?;
        return Ok(Step::Advance);
    }
    match value {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
            column_aliases,
            bound_columns,
            include_descendants,
            executable_query: _,
        } => match position {
            1 => *name = input.plan_text(memory)?,
            2 => *qualifier = input.plan_text(memory)?,
            3 => *alias = input.plan_optional_text(memory)?,
            4 => *column_aliases = input.plan_texts(memory)?,
            5 => *bound_columns = input.plan_optional_texts(memory)?,
            6 => *include_descendants = input.flag()?,
            _ => return Ok(Step::Done),
        },
        SourcePlan::Join {
            comma,
            left: _,
            right: _,
            kind,
            on,
            using,
            natural,
            alias,
            column_aliases,
            lateral,
            strategy,
        } => match position {
            1 => *comma = input.flag()?,
            2 | 3 => return Ok(Step::Child(Kind::Source)),
            4 => *kind = input.plan_join_kind()?,
            5 => *on = input.plan_optional_scalar(memory, references)?.take(),
            6 => *using = input.plan_using(memory)?,
            7 => *natural = input.flag()?,
            8 => *alias = input.plan_optional_text(memory)?,
            9 => *column_aliases = input.plan_texts(memory)?,
            10 => *lateral = input.flag()?,
            11 => *strategy = input.plan_strategy()?,
            _ => return Ok(Step::Done),
        },
        SourcePlan::Values {
            lateral,
            rows,
            alias,
            column_aliases,
            internal_relation,
            internal_column_types,
            internal_column_collations,
        } => match position {
            1 => *lateral = input.flag()?,
            2 => *rows = input.plan_rows(memory, references)?.take(),
            3 => *alias = input.plan_optional_text(memory)?,
            4 => *column_aliases = input.plan_texts(memory)?,
            5 => {
                *internal_relation = if input.flag()? {
                    Some(InternalRelationId::from_raw(u64::from_le_bytes(
                        input.binary.array()?,
                    )))
                } else {
                    None
                }
            }
            6 => *internal_column_types = input.plan_vector(memory, Input::plan_optional_type)?,
            7 => {
                *internal_column_collations =
                    input.plan_vector(memory, |input, _| input.optional_number())?;
            }
            _ => return Ok(Step::Done),
        },
        SourcePlan::Function {
            name,
            binding,
            output_name,
            relations,
            args,
            alias,
            column_aliases,
            ordinality,
            column_types,
        } => match position {
            1 => *name = input.plan_text(memory)?,
            2 => *binding = input.plan_binding(memory)?,
            3 => *output_name = input.plan_text(memory)?,
            4 => *relations = input.plan_relations(memory)?,
            5 => *args = input.plan_scalars(memory, references)?.take(),
            6 => *alias = input.plan_optional_text(memory)?,
            7 => *column_aliases = input.plan_texts(memory)?,
            8 => *ordinality = input.flag()?,
            9 => *column_types = input.plan_texts(memory)?,
            _ => return Ok(Step::Done),
        },
        SourcePlan::FunctionGroup {
            functions,
            alias,
            column_aliases,
            ordinality,
        } => match position {
            1 => *functions = input.plan_functions(memory, references)?.take(),
            2 => *alias = input.plan_optional_text(memory)?,
            3 => *column_aliases = input.plan_texts(memory)?,
            4 => *ordinality = input.flag()?,
            _ => return Ok(Step::Done),
        },
        SourcePlan::Subquery {
            lateral,
            body: _,
            alias,
            column_aliases,
            captured_output: _,
        } => match position {
            1 => *lateral = input.flag()?,
            2 => return Ok(Step::Child(Kind::Query)),
            3 => *alias = input.plan_optional_text(memory)?,
            4 => *column_aliases = input.plan_texts(memory)?,
            _ => return Ok(Step::Done),
        },
        SourcePlan::RetainedRows(_) => unreachable!("stored source constructor"),
    }
    Ok(Step::Advance)
}

fn node(input: &mut Input<'_>, memory: &mut MemoryReservation) -> Result<SourcePlan, SQLError> {
    Ok(match input.binary.array::<1>()?[0] {
        0 => SourcePlan::Table {
            name: String::new(),
            qualifier: String::new(),
            alias: None,
            column_aliases: Vec::new(),
            bound_columns: None,
            include_descendants: false,
            executable_query: None,
        },
        1 => {
            memory.grow(2 * size_of::<SourcePlan>())?;
            SourcePlan::Join {
                comma: false,
                left: Box::new(empty_source()),
                right: Box::new(empty_source()),
                kind: JoinKind::Inner,
                on: None,
                using: None,
                natural: false,
                alias: None,
                column_aliases: Vec::new(),
                lateral: false,
                strategy: JoinExecutionStrategy::Auto,
            }
        }
        2 => empty_source(),
        3 => SourcePlan::Function {
            name: String::new(),
            binding: None,
            output_name: String::new(),
            relations: None,
            args: Vec::new(),
            alias: None,
            column_aliases: Vec::new(),
            ordinality: false,
            column_types: Vec::new(),
        },
        4 => SourcePlan::FunctionGroup {
            functions: Vec::new(),
            alias: None,
            column_aliases: Vec::new(),
            ordinality: false,
        },
        5 => {
            memory.grow(size_of::<QueryPlan>())?;
            SourcePlan::Subquery {
                captured_output: None,
                lateral: false,
                body: Box::new(empty_query()),
                alias: None,
                column_aliases: Vec::new(),
            }
        }
        _ => return Err(invalid("query source constructor")),
    })
}

pub(super) fn attach(value: &mut SourcePlan, position: &mut usize, child: Node) {
    match (value, child) {
        (SourcePlan::Join { left, right, .. }, Node::Source(source)) => match *position {
            2 => **left = source,
            3 => **right = source,
            _ => unreachable!("join source slot"),
        },
        (SourcePlan::Subquery { body, .. }, Node::Query(query)) => **body = query,
        _ => unreachable!("source child constructor"),
    }
    *position += 1;
}
