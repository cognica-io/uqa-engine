//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct ServerErrorEnvelope {
    pub error: ServerErrorDetail,
    pub request_id: String,
}

#[derive(Deserialize)]
pub(crate) struct ServerErrorDetail {
    pub code: String,
    pub message: String,
}
