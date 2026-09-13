//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Nonnegative exact decimals with interruptible addition and powers of ten.

use uqa_core::memory::{Budgeted, BudgetedDeque, BudgetedVec};

use super::Context;
use crate::nori::error::invalid;
use crate::AnalysisResult;

#[derive(Debug)]
pub(super) struct Decimal {
    digits: BudgetedDeque<u8>,
    scale: usize,
}

impl Decimal {
    pub fn from_digits(
        mut digits: BudgetedDeque<u8>,
        scale: usize,
        context: &mut Context<'_, '_>,
    ) -> AnalysisResult<Self> {
        for index in 0..digits.len() / 2 {
            context.work.tick()?;
            let other = digits.len() - index - 1;
            let left = digits[index];
            digits[index] = digits[other];
            digits[other] = left;
        }
        while digits.len() > 1 && digits[digits.len() - 1] == 0 {
            context.work.tick()?;
            digits.pop_back();
        }
        Ok(Self { digits, scale })
    }

    pub fn power(power: usize, context: &mut Context<'_, '_>) -> AnalysisResult<Self> {
        context.check_digits(power + 1)?;
        let mut digits = BudgetedDeque::new(context.budget);
        digits.reserve(power + 1)?;
        for _ in 0..power {
            context.work.tick()?;
            digits.push_back(0)?;
        }
        digits.push_back(1)?;
        Ok(Self { digits, scale: 0 })
    }

    pub fn multiply_power(
        mut self,
        power: usize,
        context: &mut Context<'_, '_>,
    ) -> AnalysisResult<Self> {
        if self.scale >= power {
            self.scale -= power;
            return Ok(self);
        }
        let shift = power - self.scale;
        self.scale = 0;
        let required = self
            .digits
            .len()
            .checked_add(shift)
            .ok_or_else(|| invalid("Nori number", "coefficient size overflow"))?;
        context.check_digits(required)?;
        self.digits.reserve(shift)?;
        for _ in 0..shift {
            context.work.tick()?;
            self.digits.push_front(0)?;
        }
        Ok(self)
    }

    pub fn add(mut self, other: &Self, context: &mut Context<'_, '_>) -> AnalysisResult<Self> {
        let scale = self.scale.max(other.scale);
        let left_shift = scale - self.scale;
        let right_shift = scale - other.scale;
        let length = self
            .digits
            .len()
            .checked_add(left_shift)
            .zip(other.digits.len().checked_add(right_shift))
            .map(|(left, right)| left.max(right))
            .ok_or_else(|| invalid("Nori number", "aligned coefficient size overflow"))?;
        context.check_digits(length)?;
        self.digits.reserve(length - self.digits.len())?;
        for _ in 0..left_shift {
            context.work.tick()?;
            self.digits.push_front(0)?;
        }
        while self.digits.len() < length {
            context.work.tick()?;
            self.digits.push_back(0)?;
        }
        let mut carry = 0;
        // Touch only the added coefficient and carry chain, not every digit of an existing long sum.
        for (offset, &right) in other.digits.iter().enumerate() {
            context.work.tick()?;
            let index = right_shift + offset;
            let sum = self.digits[index] + right + carry;
            self.digits[index] = sum % 10;
            carry = sum / 10;
        }
        let mut index = right_shift + other.digits.len();
        while carry != 0 {
            context.work.tick()?;
            if index == self.digits.len() {
                context.check_digits(index + 1)?;
                self.digits.push_back(0)?;
            }
            let sum = self.digits[index] + carry;
            self.digits[index] = sum % 10;
            carry = sum / 10;
            index += 1;
        }
        self.scale = scale;
        Ok(self)
    }

    pub fn format(&self, context: &mut Context<'_, '_>) -> AnalysisResult<Budgeted<Vec<u16>>> {
        let mut significant = self.digits.len();
        while significant > 0 && self.digits[significant - 1] == 0 {
            context.work.tick()?;
            significant -= 1;
        }
        if significant == 0 {
            context.check_digits(1)?;
            let mut output = BudgetedVec::new(context.budget);
            output.push(u16::from(b'0'))?;
            let (output, memory) = output.into_parts();
            return Ok(Budgeted::new(output, memory));
        }
        let mut trim = 0;
        while trim < self.scale && trim < significant && self.digits[trim] == 0 {
            context.work.tick()?;
            trim += 1;
        }
        let scale = self.scale - trim;
        let digits = significant - trim;
        let required = if digits <= scale {
            scale.checked_add(2)
        } else {
            digits.checked_add(usize::from(scale > 0))
        }
        .ok_or_else(|| invalid("Nori number", "formatted size overflow"))?;
        context.check_digits(required)?;
        let mut output = BudgetedVec::new(context.budget);
        output.reserve(required)?;
        if digits <= scale {
            output.push(u16::from(b'0'))?;
            output.push(u16::from(b'.'))?;
            for _ in 0..scale - digits {
                context.work.tick()?;
                output.push(u16::from(b'0'))?;
            }
        }
        for index in (0..digits).rev() {
            context.work.tick()?;
            if digits > scale && scale > 0 && index + 1 == scale {
                output.push(u16::from(b'.'))?;
            }
            output.push(u16::from(b'0' + self.digits[index + trim]))?;
        }
        let (output, memory) = output.into_parts();
        Ok(Budgeted::new(output, memory))
    }
}
