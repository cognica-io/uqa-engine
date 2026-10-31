//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable deferred entries retain independently admitted metadata when attached to a mutable query constructor.

use super::Copying;
use crate::{semantics::view_rewrite::DeferredOuterRewrite, SQLError};
use uqa_core::memory::Produced;

impl Copying<'_> {
    pub(crate) fn take_deferred_rewrite(
        &mut self,
        rewrite: Produced<DeferredOuterRewrite>,
    ) -> Result<DeferredOuterRewrite, SQLError> {
        let (rewrite, memory) = rewrite.into_parts();
        let rewrite = self.control.finish(rewrite, memory)?;
        Ok(self.take(rewrite))
    }
}
