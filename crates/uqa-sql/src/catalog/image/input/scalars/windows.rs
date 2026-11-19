//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected collation and window metadata reconstruct without analysis callbacks.

use super::{boxed, invalid, payload, Fields, MemoryReservation, SQLError, ScalarExpr};
use crate::{
    ast::{FrameExclusion, FrameMode, WindowCallModifiers},
    catalog::collations::derivation::Derivation,
    ir::{ResultCollation, ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec},
};

impl Fields<'_> {
    pub(super) async fn native_scalar_collation(&mut self) -> Result<ResultCollation, SQLError> {
        let tag = self.binary.array::<1>().await?[0];
        let left = u32::from_le_bytes(self.binary.array().await?);
        let right = u32::from_le_bytes(self.binary.array().await?);
        let initial = u32::from_le_bytes(self.binary.array().await?);
        let derivation = match tag {
            0 if left == 0 && right == 0 => Derivation::None,
            1 if right == 0 => Derivation::Implicit(i64::from(left)),
            2 if right == 0 => Derivation::Explicit(i64::from(left)),
            3 => Derivation::Conflict(i64::from(left), i64::from(right)),
            _ => return Err(invalid("native scalar collation tag or unused payload")),
        };
        Ok(ResultCollation::new(derivation, initial))
    }

    pub(super) async fn native_scalar_window(
        &mut self,
        memory: &mut MemoryReservation,
        output: &mut ScalarExpr,
    ) -> Result<(), SQLError> {
        let name = payload(self.binary.text().await?, memory);
        let args = self.binary.count().await?;
        let filter = self
            .flag()
            .await?
            .then(|| boxed(ScalarExpr::Star, memory))
            .transpose()?;
        let modifiers = WindowCallModifiers {
            distinct: self.flag().await?,
            ordered: self.flag().await?,
            within_group: self.flag().await?,
        };
        let definition = if self.flag().await? {
            Some(self.binary.count().await?)
        } else {
            None
        };
        let partitions = self.binary.count().await?;
        let orders = self.binary.count().await?;
        let frame = if self.flag().await? {
            let mode = match self.binary.array::<1>().await?[0] {
                0 => FrameMode::Rows,
                1 => FrameMode::Range,
                2 => FrameMode::Groups,
                _ => return Err(invalid("native window frame mode")),
            };
            let start = self.native_frame_bound(memory).await?;
            let end = self.native_frame_bound(memory).await?;
            let between = self.flag().await?;
            let exclusion = match self.binary.array::<1>().await?[0] {
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
        *output = ScalarExpr::WindowCall {
            name,
            args: self.native_scalar_slots(args, memory).await?,
            spec: ScalarWindowSpec {
                definition,
                partition_by: self.native_scalar_slots(partitions, memory).await?,
                order_by: self
                    .slots(orders, memory, || ScalarOrder {
                        expr: ScalarExpr::Star,
                        descending: false,
                        nulls: None,
                    })
                    .await?,
                frame,
            },
            filter,
            modifiers,
        };
        Ok(())
    }

    async fn native_frame_bound(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<ScalarFrameBound, SQLError> {
        Ok(match self.binary.array::<1>().await?[0] {
            0 => ScalarFrameBound::UnboundedPreceding,
            1 => ScalarFrameBound::UnboundedFollowing,
            2 => ScalarFrameBound::CurrentRow,
            3 => ScalarFrameBound::Preceding(boxed(ScalarExpr::Star, memory)?),
            4 => ScalarFrameBound::Following(boxed(ScalarExpr::Star, memory)?),
            _ => return Err(invalid("native window frame bound")),
        })
    }
}
