//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A selected sequence generation preserves allocation position and definition identity separately.

mod catalog;
pub use catalog::{durable_sequences, CapturedSequence, CapturedSequences};
mod history;
pub use history::SequenceCaptureSource;
