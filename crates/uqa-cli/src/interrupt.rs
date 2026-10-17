//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Process signals request cancellation of the current session operation only.

use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;
use uqa_core::CancellationToken;

#[derive(Default)]
struct State {
    token: Option<CancellationToken>,
    generation: u64,
}
static STATE: Mutex<State> = Mutex::new(State {
    token: None,
    generation: 0,
});
static WAKE: Condvar = Condvar::new();
static INSTALLED: OnceLock<Result<(), String>> = OnceLock::new();
pub(super) fn install() -> Result<(), String> {
    INSTALLED
        .get_or_init(|| {
            ctrlc::set_handler(|| {
                let mut state = STATE
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.generation = state.generation.wrapping_add(1);
                if let Some(token) = &state.token {
                    token.cancel();
                }
                WAKE.notify_all();
            })
            .map_err(|e| format!("install query interrupt handler: {e}"))
        })
        .clone()
}
pub(super) struct QueryInterrupt(CancellationToken);
impl QueryInterrupt {
    pub(super) fn new(token: CancellationToken) -> Self {
        STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .token = Some(token.clone());
        Self(token)
    }
}
impl Drop for QueryInterrupt {
    fn drop(&mut self) {
        STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .token = None;
        self.0.reset();
    }
}
pub(super) fn generation() -> u64 {
    STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .generation
}
pub(super) fn wait(duration: Duration, generation: u64) -> bool {
    let state = STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (state, _) = WAKE
        .wait_timeout_while(state, duration, |state| state.generation == generation)
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.generation != generation
}
