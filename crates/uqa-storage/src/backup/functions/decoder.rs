//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    order::{Name, Order},
    BackupError, BackupFunctionKind, BackupResult, StorageReadControl, END, MAGIC,
};

enum Field {
    Header(usize),
    Kind,
    Length {
        kind: BackupFunctionKind,
        bytes: [u8; 4],
        filled: usize,
    },
    Name(Name),
    End,
}

pub(in crate::backup) struct FunctionDecoder {
    field: Field,
    order: Order,
    bytes_left: Option<u64>,
    control: StorageReadControl,
}

impl FunctionDecoder {
    pub(in crate::backup) fn new(length: Option<u64>, control: &StorageReadControl) -> Self {
        Self {
            field: Field::Header(0),
            order: Order::default(),
            bytes_left: length,
            control: control.clone(),
        }
    }

    // A completed name or fixed field ends this slice. This also lets the exposing reader request exactly the bytes it needs, without unbounded lookahead.
    pub(in crate::backup) fn remaining(&self) -> usize {
        match self.field {
            Field::Header(filled) => MAGIC.len() - filled,
            Field::Length { filled, .. } => 4 - filled,
            Field::Name(ref name) => name.length - name.bytes,
            Field::Kind | Field::End => 1,
        }
    }

    pub(in crate::backup) fn accept(&mut self, bytes: &[u8]) -> BackupResult<bool> {
        self.control.check()?;
        debug_assert!(!bytes.is_empty() && bytes.len() <= self.remaining());
        if let Some(remaining) = &mut self.bytes_left {
            *remaining = remaining
                .checked_sub(bytes.len() as u64)
                .ok_or(BackupError::Integrity)?;
        }
        match &mut self.field {
            Field::Header(filled) => {
                let next = *filled + bytes.len();
                if bytes != &MAGIC[*filled..next] {
                    return Err(BackupError::Invalid(
                        "registered SQL function section header",
                    ));
                }
                *filled = next;
                if next == MAGIC.len() {
                    self.field = Field::Kind;
                }
            }
            Field::Kind if bytes == [END] => self.field = Field::End,
            Field::Kind => {
                self.field = Field::Length {
                    kind: BackupFunctionKind::decode(bytes[0])?,
                    bytes: [0; 4],
                    filled: 0,
                };
            }
            Field::Length {
                kind,
                bytes: length,
                filled,
            } => {
                let next = *filled + bytes.len();
                length[*filled..next].copy_from_slice(bytes);
                *filled = next;
                if next == 4 {
                    let length = u32::from_le_bytes(*length) as usize;
                    if self.bytes_left.is_some_and(|left| length as u64 >= left) {
                        return Err(BackupError::Invalid(
                            "registered SQL function name exceeds section",
                        ));
                    }
                    self.field = Field::Name(Name::new(*kind, length, &self.control)?);
                }
            }
            Field::Name(name) => {
                self.order.append(name, bytes)?;
                if name.bytes == name.length {
                    let Field::Name(name) = std::mem::replace(&mut self.field, Field::Kind) else {
                        unreachable!()
                    };
                    self.order.finish(name)?;
                    return Ok(true);
                }
            }
            Field::End => {
                return Err(BackupError::Invalid(
                    "trailing registered SQL function bytes",
                ))
            }
        }
        Ok(false)
    }

    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    pub(in crate::backup) fn current(&self) -> Option<super::BackupFunctionReference<'_>> {
        self.order.current()
    }

    pub(in crate::backup) fn verify(&mut self, mut bytes: &[u8]) -> BackupResult<()> {
        while !bytes.is_empty() {
            let (field, rest) = bytes.split_at(bytes.len().min(self.remaining()));
            self.accept(field)?;
            bytes = rest;
        }
        Ok(())
    }

    pub(in crate::backup) fn finish(&self) -> BackupResult<()> {
        if matches!(self.field, Field::End)
            && self.bytes_left.is_none_or(|remaining| remaining == 0)
        {
            Ok(())
        } else {
            Err(BackupError::Invalid(
                "truncated registered SQL function section",
            ))
        }
    }
}
