//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Existing field slots retain ancestor links while constructor records retire. No cleanup allocation or recursive field drop is required.

use super::{MemoryReservation, RewriteField, RewriteMemory, ScalarMemory};

impl Drop for RewriteField {
    fn drop(&mut self) {
        if let Some(memory) = self.memory.take() {
            drop(fold(memory));
        }
    }
}

pub(super) fn fold(memory: RewriteMemory) -> Option<MemoryReservation> {
    let mut retained = None;
    let mut current = Some(memory);
    let mut pending = None;
    loop {
        if let Some(first) = current.as_mut().and_then(|memory| match memory {
            RewriteMemory::Fields { fields, .. } => fields.first_mut(),
            RewriteMemory::Scalars { .. } | RewriteMemory::Scalar(_) => None,
        }) {
            std::mem::swap(&mut first.memory, &mut pending);
            std::mem::swap(&mut current, &mut pending);
            continue;
        }
        absorb(current.take(), &mut retained);
        loop {
            let Some(RewriteMemory::Fields { fields, .. }) = pending.as_mut() else {
                debug_assert!(pending.is_none());
                return retained;
            };
            if fields.len() > 1 {
                current = fields
                    .pop()
                    .expect("remaining constructor field")
                    .memory
                    .take();
                break;
            }
            let previous = fields.first_mut().and_then(|field| field.memory.take());
            absorb(std::mem::replace(&mut pending, previous), &mut retained);
        }
    }
}

fn absorb(memory: Option<RewriteMemory>, retained: &mut Option<MemoryReservation>) {
    let Some(memory) = memory else {
        return;
    };
    let reservation = match memory {
        RewriteMemory::Scalar(memory) => memory.map(ScalarMemory::into_reservation),
        RewriteMemory::Scalars {
            mut direct,
            mut items,
        } => {
            while let Some(memory) = items.pop() {
                if let Some(memory) = memory {
                    direct.absorb(memory.into_reservation());
                }
            }
            drop(items);
            Some(direct)
        }
        RewriteMemory::Fields { direct, fields } => {
            debug_assert!(fields.iter().all(|field| field.memory.is_none()));
            // Constructor buffers disappear before their payload reservation is returned.
            drop(fields);
            Some(direct)
        }
    };
    if let Some(reservation) = reservation {
        if let Some(retained) = retained {
            retained.absorb(reservation);
        } else {
            *retained = Some(reservation);
        }
    }
}

#[cfg(test)]
mod tests;
