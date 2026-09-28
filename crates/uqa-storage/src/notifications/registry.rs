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

/// One registry row borrowed until the visitor returns; no channel or payload allocation is transferred implicitly.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NotificationQueueEntryRef<'a> {
    pub sequence: u64,
    pub process_id: i32,
    pub channel: &'a str,
    pub payload: &'a str,
}

/// Progress over accepted rows in a finite registry visit. An inspected but declined row remains at the resume boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotificationQueueScan {
    pub visited: usize,
    pub next_sequence: u64,
    /// True only when the query actually observed its end; reaching a row limit does not prove exhaustion.
    pub exhausted: bool,
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

/// Stable lexicographic registry key; the session identifier is stored big-endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct NotificationListenerKey {
    pub owner_id: [u8; 16],
    pub session_id: u64,
}

/// Fixed-width coordination metadata, independent of a listener's channel list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotificationListenerMetadata {
    pub key: NotificationListenerKey,
    pub process_id: i32,
    pub wake_port: u16,
    pub transaction_open: bool,
    pub next_sequence: u64,
    pub position: u64,
}
