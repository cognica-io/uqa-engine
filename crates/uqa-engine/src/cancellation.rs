//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::Engine;

impl Engine {
    pub fn cancel(&self) {
        self.runtime.cancellation.cancel();
    }

    /// Reset the cancellation flag so subsequent queries run
    /// normally. Call between query batches when reusing the same
    /// engine for many cancellable executions.
    pub fn reset_cancellation(&self) {
        self.runtime.cancellation.reset();
    }

    pub fn cancellation_token(&self) -> uqa_core::CancellationToken {
        self.query_runtime_view().cancellation_token()
    }

    /// compatibility alias for [`Engine::cancellation_token`].
    pub fn cancel_token(&self) -> uqa_core::CancellationToken {
        self.cancellation_token()
    }

    pub fn is_cancelled(&self) -> bool {
        self.runtime.cancellation.is_cancelled()
    }
}
