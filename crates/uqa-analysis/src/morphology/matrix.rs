//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dense connection costs in backward-major order.

use super::io::{vector, Reader};
use super::DictionaryResult;

#[derive(Debug)]
pub(crate) struct Matrix {
    pub forward: usize,
    pub backward: usize,
    pub costs: Vec<i16>,
}

impl Matrix {
    pub fn get(&self, forward: usize, backward: usize) -> Option<i16> {
        if forward >= self.forward || backward >= self.backward {
            return None;
        }
        Some(self.costs[backward * self.forward + forward])
    }

    pub fn decode(reader: &mut Reader<'_>) -> DictionaryResult<Self> {
        let forward = reader.u32()? as usize;
        let backward = reader.u32()? as usize;
        let count = forward
            .checked_mul(backward)
            .ok_or_else(|| reader.invalid("matrix size overflow"))?;
        if forward == 0
            || backward == 0
            || forward > 0x1_0000
            || backward > 0x1_0000
            || count > reader.remaining() / 2
        {
            return Err(reader.invalid("invalid connection matrix dimensions"));
        }
        let mut costs = vector(count)?;
        for _ in 0..count {
            costs.push(reader.u16()? as i16);
        }
        Ok(Self {
            forward,
            backward,
            costs,
        })
    }

    #[cfg(any(test, feature = "nori-tools", feature = "kuromoji-tools"))]
    pub fn encode(&self, output: &mut crate::morphology::io::Writer) -> DictionaryResult<()> {
        output.count(self.forward)?;
        output.count(self.backward)?;
        for cost in &self.costs {
            output.u16(*cost as u16)?;
        }
        Ok(())
    }
}
