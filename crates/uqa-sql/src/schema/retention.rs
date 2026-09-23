//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cancellable admission of immutable catalog column generations and their owned SQL payloads.

use std::sync::Arc;
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryError, MemoryReservation},
    CancellationToken, QueryCancelled, Value, ValueRetentionError,
};

use crate::ast::{ColumnDef, ColumnType, Expr, FunctionBinding};

mod columns;
mod expressions;

#[derive(Debug, thiserror::Error)]
pub enum CatalogRetentionError {
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Cancelled(#[from] QueryCancelled),
    #[error("validated column expression contains a subquery")]
    UnexpectedSubquery,
}

impl From<ValueRetentionError> for CatalogRetentionError {
    fn from(error: ValueRetentionError) -> Self {
        match error {
            ValueRetentionError::Memory(error) => Self::Memory(error),
            ValueRetentionError::Cancelled(error) => Self::Cancelled(error),
        }
    }
}

impl From<CatalogRetentionError> for crate::SQLError {
    fn from(error: CatalogRetentionError) -> Self {
        match error {
            CatalogRetentionError::Memory(error) => Self::Routine {
                sqlstate: "53200".into(),
                message: error.to_string(),
            },
            CatalogRetentionError::Cancelled(error) => Self::Cancelled(error),
            CatalogRetentionError::UnexpectedSubquery => Self::Internal(error.to_string()),
        }
    }
}

type Result<T> = std::result::Result<T, CatalogRetentionError>;

/// An admitted immutable column generation. Clones share both the original definitions and one reservation; they do not copy ASTs or reacquire an allowance. Strings, vector capacities, boxed payloads and nested Core values are charged. Allocator/reference-count bookkeeping and Core value map node slack are outside the payload allowance.
#[derive(Debug, Clone)]
pub struct RetainedColumns(Arc<Budgeted<Arc<Vec<ColumnDef>>>>);

impl RetainedColumns {
    /// Admit an existing, SQL-validated generation before acquiring shared ownership. The source remains unchanged on cancellation or quota failure. Subqueries violate the stored column-expression invariant enforced by the default, CHECK and generated-column validators and return a typed error.
    pub fn capture(
        columns: &Arc<Vec<ColumnDef>>,
        budget: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self> {
        let mut walker = Walker::new(budget, cancellation);
        walker.charge(size_of::<Vec<ColumnDef>>())?;
        walker.children(columns, Node::Column)?;
        let memory = walker.finish()?;
        cancellation.check()?;
        Ok(Self(
            Budgeted::new(Arc::clone(columns), memory).into_shared()?,
        ))
    }

    pub fn as_slice(&self) -> &[ColumnDef] {
        &self.0
    }

    pub fn reserved_bytes(&self) -> usize {
        self.0.reserved_bytes()
    }
}

impl std::ops::Deref for RetainedColumns {
    type Target = [ColumnDef];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl ColumnDef {
    /// Admit owned payloads below this inline column definition without copying them. The returned lease must remain with any retained owner.
    pub fn reserve_retained_payload(
        &self,
        budget: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<MemoryReservation> {
        Walker::new(budget, cancellation).root(Node::Column(self))
    }
}

impl ColumnType {
    /// Admit nested type names and boxed base types, excluding this type's inline layout.
    pub fn reserve_retained_payload(
        &self,
        budget: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<MemoryReservation> {
        Walker::new(budget, cancellation).root(Node::Type(self))
    }
}

impl Expr {
    /// Admit a validated column expression's owned payload, including function bindings and Core values, excluding its inline layout. Traversal is iterative and uses the same allowance. Subqueries violate the stored column-expression invariant and return a typed error.
    pub fn reserve_column_payload(
        &self,
        budget: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<MemoryReservation> {
        Walker::new(budget, cancellation).root(Node::Expr(self))
    }
}

enum Node<'a> {
    Column(&'a ColumnDef),
    Type(&'a ColumnType),
    Expr(&'a Expr),
    Binding(&'a FunctionBinding),
}

struct Walker<'a> {
    memory: MemoryReservation,
    pending: BudgetedVec<Node<'a>>,
    cancellation: &'a CancellationToken,
}

impl<'a> Walker<'a> {
    fn new(budget: &MemoryBudget, cancellation: &'a CancellationToken) -> Self {
        Self {
            memory: budget.empty_reservation(),
            pending: BudgetedVec::new(budget),
            cancellation,
        }
    }

    fn root(mut self, root: Node<'a>) -> Result<MemoryReservation> {
        self.visit(root)?;
        self.finish()
    }

    fn finish(mut self) -> Result<MemoryReservation> {
        self.cancellation.check()?;
        while let Some(node) = self.pending.pop() {
            self.visit(node)?;
        }
        Ok(self.memory)
    }

    fn visit(&mut self, node: Node<'a>) -> Result<()> {
        self.cancellation.check()?;
        match node {
            Node::Column(column) => self.column(column),
            Node::Type(ty) => self.ty(ty),
            Node::Expr(expr) => self.expr(expr),
            Node::Binding(binding) => self.binding(binding),
        }
    }

    fn node(&mut self, node: Node<'a>) -> Result<()> {
        self.cancellation.check()?;
        self.pending.push(node)?;
        Ok(())
    }

    fn charge(&mut self, bytes: usize) -> Result<()> {
        self.cancellation.check()?;
        self.memory.grow(bytes)?;
        Ok(())
    }

    fn buffer<T>(&mut self, capacity: usize) -> Result<()> {
        self.charge(
            capacity
                .checked_mul(size_of::<T>())
                .ok_or(MemoryError::SizeOverflow)?,
        )
    }

    fn children<T>(&mut self, items: &'a Vec<T>, node: fn(&'a T) -> Node<'a>) -> Result<()> {
        self.buffer::<T>(items.capacity())?;
        for item in items {
            self.node(node(item))?;
        }
        Ok(())
    }

    fn boxed<T>(&mut self, item: &'a T, node: fn(&'a T) -> Node<'a>) -> Result<()> {
        self.charge(size_of::<T>())?;
        self.node(node(item))
    }

    fn text(&mut self, text: &String) -> Result<()> {
        self.charge(text.capacity())
    }

    fn optional_text(&mut self, text: &Option<String>) -> Result<()> {
        if let Some(text) = text {
            self.text(text)?;
        }
        Ok(())
    }

    fn texts(&mut self, texts: &Vec<String>) -> Result<()> {
        self.buffer::<String>(texts.capacity())?;
        for text in texts {
            self.text(text)?;
        }
        Ok(())
    }

    fn value(&mut self, value: &Value) -> Result<()> {
        let memory = value.reserve_retained_payload(self.memory.budget(), self.cancellation)?;
        self.memory.absorb(memory);
        Ok(())
    }

    fn optional_expr(&mut self, expr: &'a Option<Expr>) -> Result<()> {
        if let Some(expr) = expr {
            self.node(Node::Expr(expr))?;
        }
        Ok(())
    }

    fn optional_boxed_expr(&mut self, expr: &'a Option<Box<Expr>>) -> Result<()> {
        if let Some(expr) = expr {
            self.boxed(expr.as_ref(), Node::Expr)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
