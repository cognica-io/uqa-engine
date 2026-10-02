//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounds for the waits of concurrent schedules, which assert an order of events and never a speed.

use std::time::Duration;

/// How long a schedule waits for a step that has to complete. The bound only ends a test whose step never completes: a loaded machine takes many times what the step takes on an idle one.
pub(crate) const COMPLETION: Duration = Duration::from_secs(60);
