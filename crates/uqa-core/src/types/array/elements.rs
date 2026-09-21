//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native row-major array traversal with an optional shared memory allowance.

use super::{ArrayValue, Value};
use crate::{
    memory::{BudgetedVec, MemoryBudget, MemoryError},
    CancellationToken, QueryCancelled,
};

#[derive(Debug, thiserror::Error)]
pub enum ArrayTraversalError {
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Cancelled(#[from] QueryCancelled),
}

enum Stack<'a> {
    Native(Vec<std::slice::Iter<'a, Value>>),
    Budgeted(BudgetedVec<std::slice::Iter<'a, Value>>),
}

impl<'a> Stack<'a> {
    fn push(&mut self, values: &'a [Value]) -> Result<(), MemoryError> {
        match self {
            Self::Native(stack) => stack.push(values.iter()),
            Self::Budgeted(stack) => stack.push(values.iter())?,
        }
        Ok(())
    }

    fn next(
        &mut self,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<&'a Value>, ArrayTraversalError> {
        loop {
            if let Some(cancellation) = cancellation {
                cancellation.check()?;
            }
            let current = match self {
                Self::Native(stack) => stack.last_mut(),
                Self::Budgeted(stack) => stack.last_mut(),
            };
            let Some(current) = current else {
                return Ok(None);
            };
            match current.next() {
                Some(Value::List(values)) => self.push(values)?,
                Some(Value::Array(array)) => self.push(array.elements())?,
                Some(value) => return Ok(Some(value)),
                None => match self {
                    Self::Native(stack) => {
                        stack.pop();
                    }
                    Self::Budgeted(stack) => {
                        stack.pop();
                    }
                },
            }
        }
    }
}

/// A borrowed row-major cursor. Its traversal stack shares the caller's allowance and checks cancellation even while passing through empty nested arrays. Discard the cursor after an error.
pub struct BudgetedArrayElements<'a> {
    stack: Stack<'a>,
    cancellation: &'a CancellationToken,
}

impl<'a> BudgetedArrayElements<'a> {
    pub fn next_element(&mut self) -> Result<Option<&'a Value>, ArrayTraversalError> {
        self.stack.next(Some(self.cancellation))
    }
}

impl ArrayValue {
    pub fn budgeted_elements<'a>(
        &'a self,
        memory: &MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<BudgetedArrayElements<'a>, ArrayTraversalError> {
        cancellation.check()?;
        let mut stack = Stack::Budgeted(BudgetedVec::new(memory));
        stack.push(self.elements())?;
        Ok(BudgetedArrayElements {
            stack,
            cancellation,
        })
    }

    pub(crate) fn flattened_elements(&self) -> impl Iterator<Item = &Value> {
        let mut stack = Stack::Native(vec![self.elements().iter()]);
        std::iter::from_fn(move || {
            stack
                .next(None)
                .expect("native array traversal is infallible")
        })
    }
}
