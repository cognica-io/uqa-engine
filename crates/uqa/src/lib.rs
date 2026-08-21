//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Convenience facade for the embedded UQA engine.
//!
//! This crate re-exports the complete `uqa_engine` API and the core [`Value`] type so applications can start with one dependency while lower-level crates remain independently usable.

pub use uqa_core::Value;
pub use uqa_engine::*;

#[cfg(test)]
mod tests {
    use super::{Engine, Value};

    #[test]
    fn reexports_engine_and_value() {
        let engine = Engine::new();
        let value = Value::Int(1);
        drop((engine, value));
    }
}
