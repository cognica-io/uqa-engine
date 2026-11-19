//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Load-only routine restoration observes the original migration selections without rewriting native syntax.

use crate::ast::{
    AssignmentStep, AssignmentTarget, AssignmentTargets, CreateFunction, CteBody, DeleteStmt, Expr,
    FromClause, FunctionBody, FunctionParam, InsertStmt, MergeStmt, MergeWhen, OrderBy, Projection,
    SelectStmt, Statement, TableFunction, UpdateStmt, WindowDefinition, WindowSpec, CTE,
};
use uqa_core::{
    image_io::{AsyncImageInput, ImageIoError},
    memory::BudgetedVec,
    CancellationToken,
};

mod expressions;
mod statements;

impl CreateFunction {
    /// Observe the same defaults and complete statement bodies as initial-open dispatch migration. Source bodies remain uninterpreted, and every continuation owns only admitted traversal frames borrowing the original declaration.
    pub async fn has_legacy_serialized_dispatches_from_input(
        &self,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<bool, ImageIoError> {
        observe(Node::Function(self), original, input).await
    }
}

struct Frame<'a> {
    node: Node<'a>,
    position: usize,
}

#[derive(Clone, Copy)]
enum Node<'a> {
    Statement(&'a Statement),
    Statements(&'a [Statement]),
    Function(&'a CreateFunction),
    Parameters(&'a [FunctionParam]),
    Expression(&'a Expr),
    Expressions(&'a [Expr]),
    Rows(&'a [Vec<Expr>]),
    Projections(&'a [Projection]),
    Orders(&'a [OrderBy]),
    Cases(&'a [(Expr, Expr)]),
    Window(&'a WindowSpec),
    Windows(&'a [WindowDefinition]),
    Select(&'a SelectStmt),
    From(&'a FromClause),
    TableFunction(&'a TableFunction),
    TableFunctions(&'a [TableFunction]),
    Ctes(&'a [CTE]),
    Cte(&'a CTE),
    Insert(&'a InsertStmt),
    Update(&'a UpdateStmt),
    Delete(&'a DeleteStmt),
    Merge(&'a MergeStmt),
    When(&'a MergeWhen),
    Whens(&'a [MergeWhen]),
    Targets(&'a [AssignmentTarget]),
    Target(&'a AssignmentTarget),
    Assignment(&'a AssignmentTargets, &'a Expr),
    Assignments(&'a [(AssignmentTargets, Expr)]),
    AssignmentStep(&'a AssignmentStep<Expr>),
}

enum Child<'a> {
    Node(Node<'a>),
    Skip,
    Done,
}

fn fixed<const N: usize>(values: [Option<Node<'_>>; N], position: usize) -> Child<'_> {
    match values.get(position) {
        Some(Some(node)) => Child::Node(*node),
        Some(None) => Child::Skip,
        None => Child::Done,
    }
}

fn item<'a, T>(
    values: &'a [T],
    position: usize,
    node: impl FnOnce(&'a T) -> Node<'a>,
) -> Child<'a> {
    values
        .get(position)
        .map_or(Child::Done, |value| Child::Node(node(value)))
}

async fn observe(
    root: Node<'_>,
    original: &CancellationToken,
    input: &mut AsyncImageInput<'_>,
) -> Result<bool, ImageIoError> {
    original.check()?;
    input.checkpoint().await?;
    original.check()?;
    let mut frames = BudgetedVec::new(input.memory);
    frames.push(Frame {
        node: root,
        position: 0,
    })?;
    while let Some(frame) = frames.last_mut() {
        original.check()?;
        input.checkpoint().await?;
        original.check()?;
        if frame.position == 0 && frame.node.needs_migration(original, input).await? {
            return Ok(true);
        }
        let child = frame.node.child(frame.position);
        frame.position += 1;
        match child {
            Child::Node(node) => frames.push(Frame { node, position: 0 })?,
            Child::Skip => {}
            Child::Done => {
                frames.pop();
            }
        }
    }
    original.check()?;
    input.cancellation.check()?;
    Ok(false)
}

impl<'a> Node<'a> {
    async fn needs_migration(
        self,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<bool, ImageIoError> {
        Ok(match self {
            Self::Expression(expression) => {
                expressions::needs_migration(expression, original, input).await?
            }
            Self::From(FromClause::Function { binding, args, .. }) => {
                crate::semantics::has_legacy_table_function_binding(binding.as_ref(), args.len())
            }
            Self::TableFunction(function) => crate::semantics::has_legacy_table_function_binding(
                function.binding.as_ref(),
                function.args.len(),
            ),
            _ => false,
        })
    }

    fn child(self, position: usize) -> Child<'a> {
        match self {
            Self::Statement(statement) => statements::statement(statement, position),
            Self::Statements(values) => item(values, position, Self::Statement),
            Self::Function(definition) => fixed(
                [
                    Some(Self::Parameters(&definition.params)),
                    match &definition.body {
                        FunctionBody::Statements(values) => Some(Self::Statements(values)),
                        FunctionBody::Source(_) => None,
                    },
                ],
                position,
            ),
            Self::Parameters(values) => match values.get(position) {
                Some(parameter) => parameter
                    .default
                    .as_ref()
                    .map_or(Child::Skip, |value| Child::Node(Self::Expression(value))),
                None => Child::Done,
            },
            Self::Expression(value) => expressions::expression(value, position),
            Self::Expressions(values) => item(values, position, Self::Expression),
            Self::Rows(values) => item(values, position, |row| Self::Expressions(row)),
            Self::Projections(values) => {
                item(values, position, |value| Self::Expression(&value.expr))
            }
            Self::Orders(values) => item(values, position, |value| Self::Expression(&value.expr)),
            Self::Cases(values) => {
                values
                    .get(position / 2)
                    .map_or(Child::Done, |(condition, result)| {
                        Child::Node(Self::Expression(if position.is_multiple_of(2) {
                            condition
                        } else {
                            result
                        }))
                    })
            }
            Self::Window(value) => expressions::window(value, position),
            Self::Windows(values) => item(values, position, |value| Self::Window(&value.spec)),
            Self::Select(value) => statements::select(value, position),
            Self::From(value) => statements::source(value, position),
            Self::TableFunction(value) => fixed([Some(Self::Expressions(&value.args))], position),
            Self::TableFunctions(values) => item(values, position, Self::TableFunction),
            Self::Ctes(values) => item(values, position, Self::Cte),
            Self::Cte(cte) => fixed(
                [
                    cte.cycle
                        .as_ref()
                        .map(|cycle| Self::Expression(&cycle.mark_value)),
                    cte.cycle
                        .as_ref()
                        .map(|cycle| Self::Expression(&cycle.mark_default)),
                    Some(match &cte.body {
                        CteBody::Query(value) => Self::Select(value),
                        CteBody::Insert(value) => Self::Insert(value),
                        CteBody::Update(value) => Self::Update(value),
                        CteBody::Delete(value) => Self::Delete(value),
                        CteBody::Merge(value) => Self::Merge(value),
                    }),
                ],
                position,
            ),
            Self::Insert(value) => statements::insert(value, position),
            Self::Update(value) => statements::update(value, position),
            Self::Delete(value) => statements::delete(value, position),
            Self::Merge(value) => statements::merge(value, position),
            Self::When(value) => statements::when(value, position),
            Self::Whens(values) => item(values, position, Self::When),
            Self::Targets(values) => item(values, position, Self::Target),
            Self::Target(value) => item(&value.indirection, position, Self::AssignmentStep),
            Self::Assignment(targets, value) => fixed(
                [
                    Some(Self::Targets(targets.targets())),
                    Some(Self::Expression(value)),
                ],
                position,
            ),
            Self::Assignments(values) => item(values, position, |(target, value)| {
                Self::Assignment(target, value)
            }),
            Self::AssignmentStep(step) => match step {
                AssignmentStep::Field(_) => Child::Done,
                AssignmentStep::Index(value) => fixed([Some(Self::Expression(value))], position),
                AssignmentStep::Slice { lower, upper } => fixed(
                    [
                        lower.as_deref().map(Self::Expression),
                        upper.as_deref().map(Self::Expression),
                    ],
                    position,
                ),
            },
        }
    }
}

#[cfg(test)]
mod tests;
