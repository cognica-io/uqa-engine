//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provider-neutral committed queue and listener records.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NotificationQueueState {
    pub next_sequence: u64,
    pub head_position: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationQueueEntry {
    pub sequence: u64,
    pub process_id: i32,
    pub channel: String,
    pub payload: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationListenerRow {
    pub owner_id: [u8; 16],
    pub session_id: u64,
    pub process_id: i32,
    pub wake_port: u16,
    pub channels: Vec<String>,
    pub transaction_open: bool,
    pub next_sequence: u64,
    pub position: u64,
}
