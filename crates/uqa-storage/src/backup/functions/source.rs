//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    address, prefix, BackupError, BackupFunctionSource, BackupResult, BackupSourceStep,
    StorageReadControl, UnitKind, END, MAGIC,
};
use crate::backup::{
    BackupCut, BackupImageSource, BackupSourceStatistics, BackupSourceUnit, OwnedBackupImage,
};

enum Stage {
    New,
    Section,
    Provider,
}

pub(super) enum Encoder {
    Header(usize),
    Next,
    Record { prefix: [u8; 5], offset: usize },
    End,
}

struct Functions<S> {
    provider: OwnedBackupImage,
    selection: Option<S>,
    final_statistics: BackupSourceStatistics,
    encoder: Encoder,
    stage: Stage,
    control: StorageReadControl,
}

impl OwnedBackupImage {
    /// Include the SQL owner's declarations for this exact provider cut. The repository writes the canonical section through its normal protected unit stream. Even an empty selection distinguishes this image from older archives.
    pub fn with_function_references<S: BackupFunctionSource + 'static>(
        self,
        source: S,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        Self::capture(control, || {
            Ok(Functions {
                provider: self,
                selection: Some(source),
                final_statistics: BackupSourceStatistics::default(),
                encoder: Encoder::Header(0),
                stage: Stage::New,
                control: control.clone(),
            })
        })
    }
}

fn emit(output: &mut [u8], input: &[u8]) -> usize {
    let bytes = output.len().min(input.len());
    output[..bytes].copy_from_slice(&input[..bytes]);
    bytes
}

impl<S: BackupFunctionSource> BackupImageSource for Functions<S> {
    fn cut(&self) -> BackupCut {
        self.provider.cut()
    }

    fn stored_view_queries(
        &self,
        limits: crate::catalog::input::CatalogInputLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Option<crate::backup::BackupViewQueryInput>> {
        self.provider.stored_view_queries(limits, control)
    }

    fn advance(&mut self, maximum_positions: usize) -> BackupResult<BackupSourceStep> {
        self.control.check()?;
        if maximum_positions == 0 {
            return Err(BackupError::Invalid("zero function image source window"));
        }
        match self.stage {
            Stage::New => {
                self.stage = Stage::Section;
                Ok(BackupSourceStep::Unit)
            }
            Stage::Section if !matches!(self.encoder, Encoder::End) => Err(BackupError::Invalid(
                "function declaration section is not consumed",
            )),
            Stage::Section | Stage::Provider => {
                self.stage = Stage::Provider;
                self.provider.advance(maximum_positions)
            }
        }
    }

    fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        match self.stage {
            Stage::New => None,
            Stage::Provider => self.provider.current_unit(),
            Stage::Section => Some(BackupSourceUnit {
                address: address(),
                kind: UnitKind::FunctionReferences,
                decoded_bytes: None,
            }),
        }
    }

    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        loop {
            if let Some(bytes) = self.read_current_bounded(output, 128)? {
                return Ok(bytes);
            }
        }
    }

    fn read_current_bounded(
        &mut self,
        output: &mut [u8],
        maximum_positions: usize,
    ) -> BackupResult<Option<usize>> {
        self.control.check()?;
        if output.is_empty() || maximum_positions == 0 {
            return Err(BackupError::Invalid("zero function image read window"));
        }
        match self.stage {
            Stage::New => Err(BackupError::Finished),
            Stage::Section => {
                let Some(selection) = &mut self.selection else {
                    return Ok(Some(0));
                };
                let bytes = self.encoder.read(selection, output, maximum_positions)?;
                if matches!(self.encoder, Encoder::End) {
                    self.final_statistics = selection.statistics();
                    self.selection = None;
                }
                Ok(bytes)
            }
            Stage::Provider => self
                .provider
                .read_current_bounded(output, maximum_positions),
        }
    }

    fn statistics(&self) -> BackupSourceStatistics {
        let provider = self.provider.statistics();
        let selection = self
            .selection
            .as_ref()
            .map_or(self.final_statistics, BackupFunctionSource::statistics);
        BackupSourceStatistics {
            visited_positions: provider
                .visited_positions
                .saturating_add(selection.visited_positions),
            copied_payload_bytes: provider
                .copied_payload_bytes
                .saturating_add(selection.copied_payload_bytes),
            metadata_read_bytes: provider
                .metadata_read_bytes
                .saturating_add(selection.metadata_read_bytes),
            excluded_units: provider
                .excluded_units
                .saturating_add(selection.excluded_units),
        }
    }
}

impl Encoder {
    pub(super) fn read(
        &mut self,
        selection: &mut dyn BackupFunctionSource,
        output: &mut [u8],
        maximum_positions: usize,
    ) -> BackupResult<Option<usize>> {
        let written = match self {
            Self::Header(offset) => {
                let written = emit(output, &MAGIC[*offset..]);
                *offset += written;
                if *offset == MAGIC.len() {
                    *self = Self::Next;
                }
                written
            }
            Self::Next => match selection.advance(maximum_positions)? {
                BackupSourceStep::Unit => {
                    let reference = selection.current().ok_or(BackupError::Invalid(
                        "missing selected SQL function declaration",
                    ))?;
                    *self = Self::Record {
                        prefix: prefix(reference)?,
                        offset: 0,
                    };
                    return Ok(None);
                }
                BackupSourceStep::Pending => return Ok(None),
                BackupSourceStep::Complete => {
                    *self = Self::End;
                    output[0] = END;
                    1
                }
            },
            Self::Record { prefix, offset } => {
                let reference = selection.current().ok_or(BackupError::Invalid(
                    "missing selected SQL function declaration",
                ))?;
                if *prefix != super::prefix(reference)? {
                    return Err(BackupError::Invalid(
                        "changed selected SQL function declaration",
                    ));
                }
                let name = reference.name.as_bytes();
                let unread = if *offset < prefix.len() {
                    &prefix[*offset..]
                } else {
                    &name[*offset - prefix.len()..]
                };
                let written = emit(output, unread);
                *offset += written;
                if *offset == prefix.len() + name.len() {
                    *self = Self::Next;
                }
                written
            }
            Self::End => 0,
        };
        Ok(Some(written))
    }
}
