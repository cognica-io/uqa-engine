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
    Native(Vec<(std::slice::Iter<'a, Value>, usize)>),
    Budgeted(BudgetedVec<(std::slice::Iter<'a, Value>, usize)>),
}

impl<'a> Stack<'a> {
    fn push(&mut self, values: &'a [Value], rank: usize) -> Result<(), MemoryError> {
        if values.is_empty() {
            return Ok(());
        }
        match self {
            Self::Native(stack) => stack.push((values.iter(), rank)),
            Self::Budgeted(stack) => stack.push((values.iter(), rank))?,
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
            let Some((current, rank)) = current else {
                return Ok(None);
            };
            let child_rank = rank.saturating_sub(1);
            match current.next() {
                Some(Value::List(values)) if child_rank != 0 => self.push(values, child_rank)?,
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
    /// Borrow one scalar element by zero-based row-major position without flattening the array or reading a retained element's payload. The traversal uses no allocation and checks the original and invoking cancellation scopes.
    pub fn element_at_row_major_with_control(
        &self,
        mut index: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Option<&Value>, ValueRetentionError> {
        control.check()?;
        let mut remaining = usize::from(!self.dimensions().is_empty());
        for length in self.dimensions() {
            control.check()?;
            remaining = remaining
                .checked_mul(*length)
                .ok_or(MemoryError::SizeOverflow)?;
        }
        if index >= remaining {
            return Ok(None);
        }
        let mut level = self.elements();
        for (dimension, length) in self.dimensions().iter().enumerate() {
            control.check()?;
            remaining /= length;
            let Some(value) = level.get(index / remaining) else {
                return Ok(None);
            };
            index %= remaining;
            if dimension + 1 == self.dimensions().len() {
                return Ok(Some(value));
            }
            let Value::List(children) = value else {
                return Ok(None);
            };
            level = children;
        }
        Ok(None)
    }

    pub fn elements_with_control<'v, 'c>(
        &'v self,
        control: &ProductionControl<'c>,
    ) -> Result<ControlledArrayElements<'v, 'c>, ValueRetentionError> {
        control.check()?;
        let mut stack = control.budget().map_or_else(
            || Stack::Native(Vec::new()),
            |memory| Stack::Budgeted(BudgetedVec::new(memory)),
        );
        stack.push(self.elements(), self.dimensions().len())?;
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
        stack.push(self.elements(), self.dimensions().len())?;
        Ok(BudgetedArrayElements {
            stack,
            cancellation,
        })
    }

    pub(crate) fn flattened_elements(&self) -> impl Iterator<Item = &Value> {
        let mut stack = Stack::Native(vec![(self.elements().iter(), self.dimensions().len())]);
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

#[cfg(test)]
mod positional_tests {
    use super::*;

    #[test]
    fn row_major_borrows_match_subscripts_with_no_allocation_and_both_cancellations() {
        let array = ArrayValue::with_lower_bounds(
            vec![
                Value::List(vec![Value::Int(11), Value::Null, Value::Int(13)]),
                Value::List(vec![Value::Int(21), Value::Int(22), Value::Int(23)]),
            ],
            vec![-3, 7],
        )
        .unwrap();
        let budget = MemoryBudget::new(0);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        for (index, subscripts) in [[-3, 7], [-3, 8], [-3, 9], [-2, 7], [-2, 8], [-2, 9]]
            .iter()
            .enumerate()
        {
            let actual = array
                .element_at_row_major_with_control(index, &control)
                .unwrap()
                .unwrap();
            assert!(std::ptr::eq(actual, array.element_at(subscripts).unwrap()));
        }
        assert_eq!(
            array
                .element_at_row_major_with_control(6, &control)
                .unwrap(),
            None
        );
        assert_eq!(
            array
                .element_at_row_major_with_control(usize::MAX, &control)
                .unwrap(),
            None
        );
        assert_eq!(budget.used(), 0);
        let empty = ArrayValue::with_lower_bounds(vec![], vec![0]).unwrap();
        assert_eq!(
            empty
                .element_at_row_major_with_control(0, &control)
                .unwrap(),
            None
        );
        for token in [&original, &invoking] {
            token.cancel();
            assert!(matches!(
                array.element_at_row_major_with_control(0, &control),
                Err(ValueRetentionError::Cancelled(_))
            ));
            assert!(matches!(
                empty.element_at_row_major_with_control(0, &control),
                Err(ValueRetentionError::Cancelled(_))
            ));
            token.reset();
        }
    }
}
