//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::ValueIndexCache;
use std::ops::{Deref, DerefMut};
use uqa_storage::memory_capture::MemoryCaptureWriteGuard;

pub enum ValueIndexRead<'a> {
    Plain(&'a ValueIndexCache),
    Captured(parking_lot::RwLockReadGuard<'a, ValueIndexCache>),
}

impl Deref for ValueIndexRead<'_> {
    type Target = ValueIndexCache;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Plain(value) => value,
            Self::Captured(value) => value,
        }
    }
}

pub enum ValueIndexWrite<'a> {
    Plain(&'a mut ValueIndexCache),
    Captured(MemoryCaptureWriteGuard<'a, ValueIndexCache>),
}

impl Deref for ValueIndexWrite<'_> {
    type Target = ValueIndexCache;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Plain(value) => value,
            Self::Captured(value) => value,
        }
    }
}

impl DerefMut for ValueIndexWrite<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Plain(value) => value,
            Self::Captured(value) => value,
        }
    }
}
