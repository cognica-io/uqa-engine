//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Descriptor and external-source bounds apply before a compiled handle is published.

use std::io::{self, Write};

use serde::Serialize;

use crate::{AnalysisError, AnalysisResult};

#[derive(Debug, Clone, Copy)]
pub struct AnalyzerLimits {
    pub max_descriptor_bytes: usize,
    pub max_stages: usize,
    pub max_cached_analyzers: usize,
    pub max_cached_descriptor_bytes: usize,
}

impl Default for AnalyzerLimits {
    fn default() -> Self {
        Self {
            max_descriptor_bytes: 16 * 1024 * 1024,
            max_stages: 256,
            max_cached_analyzers: 128,
            max_cached_descriptor_bytes: 8 * 1024 * 1024,
        }
    }
}

pub(crate) fn check_limit(
    resource: &'static str,
    required: usize,
    limit: usize,
) -> AnalysisResult<()> {
    if required > limit {
        return Err(AnalysisError::ResourceLimit {
            resource,
            required,
            limit,
        });
    }
    Ok(())
}

struct BoundedWriter {
    bytes: Vec<u8>,
    length: usize,
    maximum: usize,
    retain: bool,
    exceeded: Option<usize>,
}

impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let required = self
            .length
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("analyzer size overflow"))?;
        if required > self.maximum {
            self.exceeded = Some(required);
            return Err(io::Error::other("analyzer descriptor exceeds its limit"));
        }
        if self.retain {
            self.bytes.extend_from_slice(bytes);
        }
        self.length = required;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn encode(
    value: &impl Serialize,
    maximum: usize,
    retain: bool,
) -> AnalysisResult<Vec<u8>> {
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        length: 0,
        maximum,
        retain,
        exceeded: None,
    };
    let result = serde_json::to_writer(&mut writer, value);
    if let Some(required) = writer.exceeded {
        return Err(AnalysisError::ResourceLimit {
            resource: "analyzer descriptor bytes",
            required,
            limit: maximum,
        });
    }
    result?;
    Ok(writer.bytes)
}
