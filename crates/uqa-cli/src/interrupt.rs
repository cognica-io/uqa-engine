//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Process signals cancel session operations and stop non-interactive input.

use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;
use uqa_core::CancellationToken;

#[derive(Default)]
struct State {
    token: Option<CancellationToken>,
    generation: u64,
    waiting_for_input: bool,
    input_generation: Option<u64>,
}
static STATE: Mutex<State> = Mutex::new(State {
    token: None,
    generation: 0,
    waiting_for_input: false,
    input_generation: None,
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
                if state.waiting_for_input && state.token.is_none() {
                    // psql exits with 3 when SIGINT interrupts non-interactive input.
                    std::process::exit(3);
                }
                if let Some(token) = &state.token {
                    token.cancel();
                }
                WAKE.notify_all();
            })
            .map_err(|e| format!("install query interrupt handler: {e}"))
        })
        .clone()
}
pub(super) struct InputWait;
impl InputWait {
    pub(super) fn enter() -> Self {
        STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .waiting_for_input = true;
        Self
    }
}
impl Drop for InputWait {
    fn drop(&mut self) {
        STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .waiting_for_input = false;
    }
}
pub(super) struct QueryInterrupt(CancellationToken);
impl QueryInterrupt {
    pub(super) fn new(token: CancellationToken) -> Result<Self, String> {
        let mut state = STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .input_generation
            .is_some_and(|generation| generation != state.generation)
        {
            return Err("57014: canceling input due to user request".into());
        }
        state.token = Some(token.clone());
        Ok(Self(token))
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
/// Keep interrupts that arrive between statements pending until their input unit returns.
pub(super) struct InputScope {
    previous: Option<u64>,
    pub(super) generation: u64,
}
impl InputScope {
    pub(super) fn enter() -> Self {
        let mut state = STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = state.input_generation;
        let generation = previous.unwrap_or(state.generation);
        state.input_generation = Some(generation);
        Self {
            previous,
            generation,
        }
    }
}
impl Drop for InputScope {
    fn drop(&mut self) {
        STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .input_generation = self.previous;
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
