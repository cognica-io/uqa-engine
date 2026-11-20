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
    memory::{MemoryBudget, MemoryError, ProductionControl},
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
        let mut nodes = BorrowedNodes::new(Node::Function(self), input, original).await?;
        while let Some(node) = nodes.next(original, input).await? {
            if node.needs_migration(original, input).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// The original children of a stored expression, including schema-specific boundaries that the consumer can reject before entering a nested query.
pub(crate) struct StoredExpressionNodes<'a>(BorrowedNodes<'a>);

impl<'a> StoredExpressionNodes<'a> {
    pub(crate) async fn next(
        &mut self,
        control: &ProductionControl<'_>,
        work: &mut impl crate::catalog::work::Work,
    ) -> Result<Option<&'a Expr>, ImageIoError> {
        self.next_expression(control, work, true).await
    }

    /// A lowered scalar owns its original operands. Relational subqueries have a separate arena.
    pub(crate) async fn next_scalar(
        &mut self,
        control: &ProductionControl<'_>,
        work: &mut impl crate::catalog::work::Work,
    ) -> Result<Option<&'a Expr>, ImageIoError> {
        self.next_expression(control, work, false).await
    }

    async fn next_expression(
        &mut self,
        control: &ProductionControl<'_>,
        work: &mut impl crate::catalog::work::Work,
        include_subqueries: bool,
    ) -> Result<Option<&'a Expr>, ImageIoError> {
        while let Some(node) = self.0.next_with_work(control, work).await? {
            match node {
                Node::Expression(expression) => return Ok(Some(expression)),
                Node::Select(_) if !include_subqueries => {
                    self.0.frames.pop();
                }
                _ => (),
            }
        }
        Ok(None)
    }
}

impl Expr {
    pub(crate) fn stored_nodes(
        &self,
        memory: &MemoryBudget,
    ) -> Result<StoredExpressionNodes<'_>, MemoryError> {
        self.stored_nodes_with_budget(Some(memory))
    }

    pub(crate) fn stored_nodes_with_budget(
        &self,
        memory: Option<&MemoryBudget>,
    ) -> Result<StoredExpressionNodes<'_>, MemoryError> {
        Ok(StoredExpressionNodes(BorrowedNodes::root(
            Node::Expression(self),
            memory,
        )?))
    }

    /// Observe the scalar registry lookups of a stored schema expression. An explicit stored binding keeps its selected implementation.
    pub async fn visit_runtime_scalar_calls<'a, 'input: 'a, F>(
        &'a self,
        original: &'a CancellationToken,
        input: &'a mut AsyncImageInput<'input>,
        visit: &'a mut F,
    ) -> Result<(), crate::SQLError>
    where
        F: FnMut(
                &str,
                crate::binding::view_dependencies::StoredFunctionSite,
            ) -> Result<(), crate::SQLError>
            + Send,
    {
        use crate::binding::view_dependencies::StoredFunctionSite;
        self.visit_stored_scalar_calls_from_input(original, input, &mut |name, binding| {
            if let Some(site) = StoredFunctionSite::ScalarOnly.with_stored_binding(binding) {
                visit(name, site)?;
            }
            Ok(())
        })
        .await
    }

    /// Legacy rewrite predicates retain their SQL placements until ordinary binding installs a condition plan.
    pub(crate) async fn visit_runtime_query_calls(
        &self,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
        visit: &mut (impl FnMut(
            &str,
            crate::binding::view_dependencies::StoredFunctionSite,
        ) -> Result<(), crate::SQLError>
                  + Send),
    ) -> Result<(), crate::SQLError> {
        use crate::binding::view_dependencies::StoredFunctionSite as Site;
        let mut nodes = BorrowedNodes::new(Node::Expression(self), input, original).await?;
        while let Some(node) = nodes.next(original, input).await? {
            let (name, site, binding) = match node {
                Node::Expression(Self::Func { name, binding, .. }) => {
                    (name, Site::Expression, binding.as_ref())
                }
                Node::Expression(Self::WindowCall { name, .. }) => (name, Site::Window, None),
                Node::From(FromClause::Function { name, binding, .. })
                | Node::TableFunction(TableFunction { name, binding, .. }) => {
                    (name, Site::From, binding.as_ref())
                }
                _ => continue,
            };
            if let Some(site) = site.with_stored_binding(binding) {
                visit(name, site)?;
            }
        }
        Ok(())
    }

    /// Visit scalar function calls in stored syntax without evaluating them or copying their children. The original and invoking controls govern every traversal frame.
    pub(crate) async fn visit_stored_scalar_calls_from_input(
        &self,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
        visit: &mut (impl FnMut(&str, Option<&crate::ast::FunctionBinding>) -> Result<(), crate::SQLError>
                  + Send),
    ) -> Result<(), crate::SQLError> {
        let mut nodes = BorrowedNodes::new(Node::Expression(self), input, original).await?;
        while let Some(node) = nodes.next(original, input).await? {
            if let Node::Expression(Self::Func { name, binding, .. }) = node {
                visit(name, binding.as_ref())?;
            }
        }
        Ok(())
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

struct BorrowedNodes<'a> {
    frames: crate::construction::Stack<Frame<'a>, 4>,
    entering: bool,
}

impl<'a> BorrowedNodes<'a> {
    async fn new(
        root: Node<'a>,
        input: &mut AsyncImageInput<'_>,
        original: &CancellationToken,
    ) -> Result<Self, ImageIoError> {
        original.check()?;
        input.checkpoint().await?;
        original.check()?;
        Ok(Self::root(root, Some(input.memory))?)
    }

    fn root(root: Node<'a>, memory: Option<&MemoryBudget>) -> Result<Self, MemoryError> {
        let mut frames = crate::construction::Stack::with_budget(memory);
        frames.try_push(Frame {
            node: root,
            position: 0,
        })?;
        Ok(Self {
            frames,
            entering: true,
        })
    }

    async fn next(
        &mut self,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Option<Node<'a>>, ImageIoError> {
        let control = ProductionControl::new(input.memory, original, input.cancellation);
        self.next_with_work(&control, input).await
    }

    async fn next_with_work(
        &mut self,
        control: &ProductionControl<'_>,
        work: &mut impl crate::catalog::work::Work,
    ) -> Result<Option<Node<'a>>, ImageIoError> {
        while let Some(frame) = self.frames.last_mut() {
            if self.entering || frame.position != 0 {
                control.check_cancellation()?;
                work.checkpoint().await?;
                control.check_cancellation()?;
            }
            if self.entering {
                self.entering = false;
                return Ok(Some(frame.node));
            }
            let child = frame.node.child(frame.position);
            frame.position += 1;
            match child {
                Child::Node(node) => {
                    let next = Frame { node, position: 0 };
                    if matches!(frame.node.child(frame.position), Child::Done) {
                        *frame = next;
                    } else {
                        self.frames.try_push(next)?;
                    }
                    self.entering = true;
                }
                Child::Skip => {}
                Child::Done => {
                    self.frames.pop();
                }
            }
        }
        control.check_cancellation()?;
        Ok(None)
    }
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
