//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Field, Input, Result, Window};
use crate::catalog::work::{ReferenceWork, Work};
use crate::construction;
use std::cmp::Ordering;
use uqa_core::memory::{MapKeyInput, OwnedMap, OwnedSet, ProductionControl};

impl<T: Field + Ord> Field for OwnedSet<T>
where
    for<'a, 'b, 'c> ReferenceWork<'a, 'b, construction::Work<'c>>:
        MapKeyInput<T, Error = crate::SQLError>,
{
    fn fits(&self, _: &mut Window) -> bool {
        self.is_empty()
    }

    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        let mut output = Self::new();
        for value in self {
            input.work.checkpoint().await;
            input.copying.reserve(Self::entry_bytes())?;
            let key = input.field(value).await?;
            let key = ProductionControl::uncontrolled().finish(key, None)?;
            let control = input.control();
            let mut comparison = ReferenceWork {
                control: &control,
                work: &mut input.work,
            };
            output
                .insert_produced_key_from(key, &mut comparison)
                .await?;
        }
        Ok(output)
    }
}

impl<V: Field> Field for OwnedMap<String, V> {
    fn fits(&self, _: &mut Window) -> bool {
        self.is_empty()
    }

    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        let mut output = Self::new();
        for (key, value) in self {
            input.work.checkpoint().await;
            input.copying.reserve(Self::entry_bytes())?;
            let key = input.field(key).await?;
            let value = input.field(value).await?;
            let key = ProductionControl::uncontrolled().finish(key, None)?;
            let control = input.control();
            let mut comparison = ReferenceWork {
                control: &control,
                work: &mut input.work,
            };
            output
                .insert_produced_key_from(key, value, &mut comparison)
                .await?;
        }
        Ok(output)
    }
}

impl<W: Work + Send> MapKeyInput<String> for ReferenceWork<'_, '_, W> {
    type Error = crate::SQLError;

    async fn checkpoint(&mut self) -> Result<()> {
        Self::checkpoint(self).await
    }

    async fn compare(&mut self, left: &String, right: &String) -> Result<Ordering> {
        MapKeyInput::<String, &str>::compare(self, left, &right.as_str()).await
    }
}

macro_rules! fixed_key {
    ($($ty:ty),+) => { $(
        impl<W: Work + Send> MapKeyInput<$ty> for ReferenceWork<'_, '_, W> {
            type Error = crate::SQLError;
            async fn checkpoint(&mut self) -> Result<()> {
                Self::checkpoint(self).await
            }
            async fn compare(&mut self, left: &$ty, right: &$ty) -> Result<Ordering> {
                self.checkpoint().await?;
                Ok(left.cmp(right))
            }
        }
    )+ };
}
fixed_key!(usize, u32);
