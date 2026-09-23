//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native row-major array traversal with an optional shared memory allowance.

use super::{ArrayValue, Value};
use crate::{
    memory::{BudgetedVec, MemoryBudget, MemoryError, ProductionControl},
    CancellationToken, QueryCancelled, ValueRetentionError,
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
        if values.is_empty() {
            return Ok(());
        }
        match self {
            Self::Native(stack) => stack.push(values.iter()),
            Self::Budgeted(stack) => stack.push(values.iter())?,
        }
        Ok(())
    }

    fn next(
        &mut self,
        check: &mut impl FnMut() -> Result<(), QueryCancelled>,
    ) -> Result<Option<&'a Value>, ArrayTraversalError> {
        loop {
            check()?;
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
        self.stack.next(&mut || self.cancellation.check())
    }
}

pub struct ControlledArrayElements<'v, 'c> {
    stack: Stack<'v>,
    control: ProductionControl<'c>,
}

impl<'v> ControlledArrayElements<'v, '_> {
    pub fn next_element(&mut self) -> Result<Option<&'v Value>, ValueRetentionError> {
        self.stack
            .next(&mut || self.control.check_cancellation())
            .map_err(|error| match error {
                ArrayTraversalError::Memory(error) => error.into(),
                ArrayTraversalError::Cancelled(error) => error.into(),
            })
    }
}

impl ArrayValue {
    pub fn elements_with_control<'v, 'c>(
        &'v self,
        control: &ProductionControl<'c>,
    ) -> Result<ControlledArrayElements<'v, 'c>, ValueRetentionError> {
        control.check()?;
        let mut stack = control.budget().map_or_else(
            || Stack::Native(Vec::new()),
            |memory| Stack::Budgeted(BudgetedVec::new(memory)),
        );
        stack.push(self.elements())?;
        Ok(ControlledArrayElements {
            stack,
            control: *control,
        })
    }

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
                .next(&mut || Ok(()))
                .expect("native array traversal is infallible")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_traversal_needs_no_stack_and_nonempty_readers_release_their_stack() {
        let token = CancellationToken::new();
        let empty = ArrayValue::try_new(Vec::new()).unwrap();
        let budget = MemoryBudget::new(0);
        let control = ProductionControl::new(&budget, &token, &token);
        let mut reader = empty.elements_with_control(&control).unwrap();
        assert!(reader.next_element().unwrap().is_none());
        assert_eq!(budget.used(), 0);
        let nonempty = ArrayValue::try_new(vec![Value::Int(7)]).unwrap();
        assert!(matches!(
            nonempty.elements_with_control(&control),
            Err(ValueRetentionError::Memory(_))
        ));
        assert_eq!(budget.used(), 0);
        let budget = MemoryBudget::new(4096);
        let control = ProductionControl::new(&budget, &token, &token);
        let mut reader = nonempty.elements_with_control(&control).unwrap();
        assert!(budget.used() > 0);
        assert_eq!(reader.next_element().unwrap(), Some(&Value::Int(7)));
        token.cancel();
        assert!(matches!(
            reader.next_element(),
            Err(ValueRetentionError::Cancelled(_))
        ));
        drop(reader);
        assert_eq!(budget.used(), 0);
    }
}
