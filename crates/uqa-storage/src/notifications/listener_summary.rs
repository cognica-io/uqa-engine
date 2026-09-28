//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact fixed-workspace summaries over live registry metadata.

use super::NotificationListenerMetadata;
use crate::StorageBackendResult;

/// A deduplicated set over the complete 16-bit wake-port domain. Its optional bitmap has exactly 1,024 words, independent of listener count.
#[derive(Default)]
pub struct NotificationWakePorts {
    bits: Option<Box<[u64]>>,
}

impl NotificationWakePorts {
    pub fn insert(&mut self, port: u16) -> StorageBackendResult<()> {
        if self.bits.is_none() {
            let mut bits = Vec::new();
            bits.try_reserve_exact(1_024)
                .map_err(uqa_core::memory::MemoryError::from)?;
            bits.resize(1_024, 0);
            self.bits = Some(bits.into_boxed_slice());
        }
        let index = usize::from(port);
        self.bits.as_mut().expect("bitmap reserved")[index / 64] |= 1_u64 << (index % 64);
        Ok(())
    }

    pub fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        self.bits
            .iter()
            .flat_map(|bits| bits.iter().enumerate())
            .flat_map(|(index, word)| {
                let mut remaining = *word;
                std::iter::from_fn(move || {
                    if remaining == 0 {
                        return None;
                    }
                    let bit = usize::try_from(remaining.trailing_zeros()).expect("bit below 64");
                    remaining &= remaining - 1;
                    Some(u16::try_from(index * 64 + bit).expect("16-bit bitmap index"))
                })
            })
    }
}

/// Minima and optional remote wake destinations for the same live listener set. No channel strings or per-listener vector are retained.
#[derive(Default)]
pub struct NotificationListenerSummary {
    pub next_sequence: Option<u64>,
    pub oldest: Option<NotificationListenerMetadata>,
    pub wake_ports: NotificationWakePorts,
}

impl NotificationListenerSummary {
    pub fn include(
        &mut self,
        listener: NotificationListenerMetadata,
        wake_remote: bool,
    ) -> StorageBackendResult<()> {
        if wake_remote {
            self.wake_ports.insert(listener.wake_port)?;
        }
        self.next_sequence = Some(
            self.next_sequence
                .map_or(listener.next_sequence, |sequence| {
                    sequence.min(listener.next_sequence)
                }),
        );
        if self
            .oldest
            .is_none_or(|oldest| (listener.position, listener.key) < (oldest.position, oldest.key))
        {
            self.oldest = Some(listener);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{NotificationListenerMetadata, NotificationListenerSummary, NotificationWakePorts};
    use crate::notifications::NotificationListenerKey;

    #[test]
    fn summary_preserves_minima_tie_order_and_exact_port_union() {
        let mut summary = NotificationListenerSummary::default();
        assert!(summary.oldest.is_none());
        assert!(summary.next_sequence.is_none());
        assert_eq!(summary.wake_ports.iter().count(), 0);
        for (owner, sequence, position, port, remote) in [
            (3, 4, 20, u16::MAX, true),
            (2, 2, 10, 64, true),
            (1, 3, 10, 1, false),
            (4, 1, 30, 64, true),
        ] {
            summary
                .include(
                    NotificationListenerMetadata {
                        key: NotificationListenerKey {
                            owner_id: [owner; 16],
                            session_id: u64::MAX,
                        },
                        process_id: i32::from(owner),
                        wake_port: port,
                        transaction_open: false,
                        next_sequence: sequence,
                        position,
                    },
                    remote,
                )
                .unwrap();
        }
        assert_eq!(summary.next_sequence, Some(1));
        assert_eq!(summary.oldest.unwrap().process_id, 1);
        assert_eq!(
            summary.wake_ports.iter().collect::<Vec<_>>(),
            [64, u16::MAX]
        );
        let mut all = NotificationWakePorts::default();
        for port in (0..=u16::MAX).rev() {
            all.insert(port).unwrap();
        }
        assert!(all.iter().eq(0..=u16::MAX));
        assert_eq!(all.bits.as_ref().unwrap().len(), 1_024);
    }
}
