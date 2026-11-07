//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected collation and window metadata reconstruct without analysis callbacks.

use super::{boxed, invalid, payload, Input, MemoryReservation, SQLError, ScalarExpr};
use crate::{
    ast::{FrameExclusion, FrameMode, WindowCallModifiers},
    catalog::collations::derivation::Derivation,
    ir::{ResultCollation, ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec},
};

impl Input<'_> {
    pub(super) fn native_scalar_collation(&mut self) -> Result<ResultCollation, SQLError> {
        let tag = self.binary.array::<1>()?[0];
        let left = u32::from_le_bytes(self.binary.array()?);
        let right = u32::from_le_bytes(self.binary.array()?);
        let initial = u32::from_le_bytes(self.binary.array()?);
        let derivation = match tag {
            0 if left == 0 && right == 0 => Derivation::None,
            1 if right == 0 => Derivation::Implicit(i64::from(left)),
            2 if right == 0 => Derivation::Explicit(i64::from(left)),
            3 => Derivation::Conflict(i64::from(left), i64::from(right)),
            _ => return Err(invalid("native scalar collation tag or unused payload")),
        };
        Ok(ResultCollation::new(derivation, initial))
    }

    pub(super) fn native_scalar_window(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<ScalarExpr, SQLError> {
        let name = payload(self.binary.text()?, memory);
        let args = self.binary.count()?;
        let filter = self
            .flag()?
            .then(|| boxed(ScalarExpr::Star, memory))
            .transpose()?;
        let modifiers = WindowCallModifiers {
            distinct: self.flag()?,
            ordered: self.flag()?,
            within_group: self.flag()?,
        };
        let definition = self.flag()?.then(|| self.binary.count()).transpose()?;
        let partitions = self.binary.count()?;
        let orders = self.binary.count()?;
        let frame = if self.flag()? {
            let mode = match self.binary.array::<1>()?[0] {
                0 => FrameMode::Rows,
                1 => FrameMode::Range,
                2 => FrameMode::Groups,
                _ => return Err(invalid("native window frame mode")),
            };
            let start = self.native_frame_bound(memory)?;
            let end = self.native_frame_bound(memory)?;
            let between = self.flag()?;
            let exclusion = match self.binary.array::<1>()?[0] {
                0 => FrameExclusion::NoOthers,
                1 => FrameExclusion::CurrentRow,
                2 => FrameExclusion::Group,
                3 => FrameExclusion::Ties,
                _ => return Err(invalid("native window frame exclusion")),
            };
            Some(ScalarWindowFrame {
                mode,
                start,
                end,
                between,
                exclusion,
            })
        } else {
            None
        };
        Ok(ScalarExpr::WindowCall {
            name,
            args: self.native_scalar_slots(args, memory)?,
            spec: ScalarWindowSpec {
                definition,
                partition_by: self.native_scalar_slots(partitions, memory)?,
                order_by: self.items(orders, memory, |_, _| {
                    Ok(ScalarOrder {
                        expr: ScalarExpr::Star,
                        descending: false,
                        nulls: None,
                    })
                })?,
                frame,
            },
            filter,
            modifiers,
        })
    }

    fn native_frame_bound(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<ScalarFrameBound, SQLError> {
        Ok(match self.binary.array::<1>()?[0] {
            0 => ScalarFrameBound::UnboundedPreceding,
            1 => ScalarFrameBound::UnboundedFollowing,
            2 => ScalarFrameBound::CurrentRow,
            3 => ScalarFrameBound::Preceding(boxed(ScalarExpr::Star, memory)?),
            4 => ScalarFrameBound::Following(boxed(ScalarExpr::Star, memory)?),
            _ => return Err(invalid("native window frame bound")),
        })
    }
}
