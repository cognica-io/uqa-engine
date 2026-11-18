//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::backup::ImageDigest;
use sha2::{Digest, Sha256};

use super::{
    source::Encoder, BackupError, BackupFunctionSource, BackupResult, FunctionDecoder,
    StorageReadControl,
};

struct Input<S> {
    source: S,
    encoder: Encoder,
    decoder: FunctionDecoder,
    hash: Sha256,
}

/// Recompute the private target's commitment through the same bounded encoder and validation used by repository capture.
pub struct BackupFunctionDigestJob<S> {
    input: Option<Input<S>>,
    complete: Option<ImageDigest>,
    control: StorageReadControl,
}

impl<S: BackupFunctionSource> BackupFunctionDigestJob<S> {
    pub fn new(source: S, control: &StorageReadControl) -> Self {
        Self {
            input: Some(Input {
                source,
                encoder: Encoder::Header(0),
                decoder: FunctionDecoder::new(None, control),
                hash: Sha256::new(),
            }),
            complete: None,
            control: control.clone(),
        }
    }

    pub fn advance(
        &mut self,
        maximum_positions: usize,
        maximum_bytes: usize,
    ) -> BackupResult<Option<ImageDigest>> {
        if self.complete.is_some() {
            return Ok(self.complete);
        }
        let mut input = self.input.take().ok_or(BackupError::Finished)?;
        self.control.check()?;
        if maximum_positions == 0 || maximum_bytes == 0 {
            return Err(BackupError::Invalid(
                "zero registered SQL function digest window",
            ));
        }
        let mut buffer = [0; 4096];
        let size = buffer.len().min(maximum_bytes);
        match input
            .encoder
            .read(&mut input.source, &mut buffer[..size], maximum_positions)?
        {
            Some(0) => {
                input.decoder.finish()?;
                self.complete = Some(ImageDigest::from_bytes(input.hash.finalize().into()));
                return Ok(self.complete);
            }
            Some(bytes) => {
                input.hash.update(&buffer[..bytes]);
                input.decoder.verify(&buffer[..bytes])?;
            }
            None => (),
        }
        self.input = Some(input);
        Ok(None)
    }
}
