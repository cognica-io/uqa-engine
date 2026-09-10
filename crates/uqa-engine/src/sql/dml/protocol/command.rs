//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Command data and engine-owned transaction scope adapters.

pub(crate) use crate::transactions::{run_mutation_command, MutationOverlayScope};
pub(crate) use uqa_execution::mutation::overlay::{
    CommandExactIndex, CommandMutationOverlay, CommandStoredDocument,
};
