//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reserved temporal endpoints share the existing numeric carrier and durable layout.

use super::TemporalValue;

// Timestamp payloads use PostgreSQL's epoch and the same two reserved endpoints.
const TIMESTAMP_NEGATIVE_INFINITY: i64 = i64::MIN;
const TIMESTAMP_POSITIVE_INFINITY: i64 = i64::MAX;

/// `PostgreSQL`'s two infinite temporal endpoints. Time-of-day types have neither endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemporalInfinity {
    Negative,
    Positive,
}

impl TemporalInfinity {
    pub const fn negate(self) -> Self {
        match self {
            Self::Negative => Self::Positive,
            Self::Positive => Self::Negative,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Negative => "-infinity",
            Self::Positive => "infinity",
        }
    }

    const fn days(self) -> i32 {
        match self {
            Self::Negative => i32::MIN,
            Self::Positive => i32::MAX,
        }
    }

    const fn micros(self) -> i64 {
        match self {
            Self::Negative => i64::MIN,
            Self::Positive => i64::MAX,
        }
    }

    pub const fn date(self) -> TemporalValue {
        TemporalValue::Date { days: self.days() }
    }

    pub const fn timestamp(self) -> TemporalValue {
        TemporalValue::Timestamp {
            postgres_micros: self.timestamp_micros(),
        }
    }

    pub const fn timestamp_tz(self) -> TemporalValue {
        TemporalValue::TimestampTz {
            postgres_micros: self.timestamp_micros(),
        }
    }

    pub const fn interval(self) -> TemporalValue {
        TemporalValue::Interval {
            months: self.days(),
            days: self.days(),
            micros: self.micros(),
        }
    }

    const fn timestamp_micros(self) -> i64 {
        match self {
            Self::Negative => TIMESTAMP_NEGATIVE_INFINITY,
            Self::Positive => TIMESTAMP_POSITIVE_INFINITY,
        }
    }
}

impl TemporalValue {
    /// Decode only the reserved endpoints, never a large finite interval or time of day.
    pub const fn infinity(&self) -> Option<TemporalInfinity> {
        use TemporalInfinity::{Negative, Positive};
        match self {
            Self::Date { days: i32::MIN }
            | Self::Timestamp {
                postgres_micros: TIMESTAMP_NEGATIVE_INFINITY,
            }
            | Self::TimestampTz {
                postgres_micros: TIMESTAMP_NEGATIVE_INFINITY,
            }
            | Self::Interval {
                months: i32::MIN,
                days: i32::MIN,
                micros: i64::MIN,
            } => Some(Negative),
            Self::Date { days: i32::MAX }
            | Self::Timestamp {
                postgres_micros: TIMESTAMP_POSITIVE_INFINITY,
            }
            | Self::TimestampTz {
                postgres_micros: TIMESTAMP_POSITIVE_INFINITY,
            }
            | Self::Interval {
                months: i32::MAX,
                days: i32::MAX,
                micros: i64::MAX,
            } => Some(Positive),
            _ => None,
        }
    }

    pub const fn is_finite(&self) -> bool {
        self.infinity().is_none()
    }

    pub(super) fn timestamp_rank(&self, micros: i64) -> i128 {
        match self.infinity() {
            Some(TemporalInfinity::Positive) => i128::from(i64::MAX) + 1,
            Some(TemporalInfinity::Negative) => i128::from(i64::MIN),
            None => i128::from(micros),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{memory::ProductionControl, Value};

    fn key(value: &TemporalValue, equality: bool) -> Vec<u8> {
        let mut key = Vec::new();
        let write = |bytes: &[u8]| {
            key.extend_from_slice(bytes);
            Ok::<_, ()>(())
        };
        if equality {
            value.write_equality_key(write).unwrap();
        } else {
            value.write_comparison_key(write).unwrap();
        }
        key
    }

    #[test]
    fn infinite_endpoints_preserve_order_keys_and_durable_carriers() {
        for (negative, finite, positive) in [
            (
                TemporalInfinity::Negative.date(),
                TemporalValue::Date { days: 0 },
                TemporalInfinity::Positive.date(),
            ),
            (
                TemporalInfinity::Negative.timestamp(),
                TemporalValue::Timestamp { postgres_micros: 0 },
                TemporalInfinity::Positive.timestamp(),
            ),
            (
                TemporalInfinity::Negative.timestamp_tz(),
                TemporalValue::TimestampTz { postgres_micros: 0 },
                TemporalInfinity::Positive.timestamp_tz(),
            ),
            (
                TemporalInfinity::Negative.interval(),
                TemporalValue::Interval {
                    months: 0,
                    days: 0,
                    micros: 0,
                },
                TemporalInfinity::Positive.interval(),
            ),
        ] {
            assert!(negative < finite && finite < positive);
            assert!(key(&negative, false) < key(&finite, false));
            assert!(key(&finite, false) < key(&positive, false));
            for endpoint in [negative, positive] {
                let text = endpoint.to_sql_string();
                let restored = endpoint.parse_same_kind(&text).unwrap();
                assert_eq!(restored, endpoint);
                assert_eq!(key(&restored, true), key(&endpoint, true));
                let json = serde_json::to_string(&Value::Temporal(endpoint.clone())).unwrap();
                assert_eq!(
                    serde_json::from_str::<Value>(&json).unwrap(),
                    Value::Temporal(endpoint.clone())
                );
                assert!(
                    !json.contains("infinity"),
                    "the numeric durable representation is retained"
                );
                assert_eq!(
                    endpoint
                        .to_sql_string_with_control(&ProductionControl::uncontrolled())
                        .unwrap()
                        .as_str(),
                    text
                );
            }
        }
        for finite in [
            TemporalValue::Timestamp {
                postgres_micros: i64::MAX - 1,
            },
            TemporalValue::TimestampTz {
                postgres_micros: i64::MAX - 1,
            },
            TemporalValue::Time { micros: i64::MAX },
            TemporalValue::TimeTz {
                micros: i64::MIN,
                offset_seconds: 0,
            },
            TemporalValue::Interval {
                months: i32::MAX,
                days: i32::MAX,
                micros: i64::MAX - 1,
            },
            TemporalValue::Interval {
                months: i32::MIN,
                days: i32::MIN,
                micros: i64::MIN + 1,
            },
        ] {
            assert!(finite.is_finite());
        }
        assert!(
            TemporalValue::Timestamp {
                postgres_micros: i64::MAX - 1
            } < TemporalInfinity::Positive.timestamp()
        );
        assert!(
            key(
                &TemporalValue::Timestamp {
                    postgres_micros: i64::MAX - 1
                },
                false
            ) < key(&TemporalInfinity::Positive.timestamp(), false)
        );
        assert!(!TemporalValue::timestamp_micros_in_range(
            TIMESTAMP_NEGATIVE_INFINITY
        ));
        assert!(!TemporalValue::timestamp_micros_in_range(
            TIMESTAMP_POSITIVE_INFINITY
        ));
    }

    #[test]
    fn infinite_temporal_producers_preserve_quota_and_cancellation() {
        use crate::{memory::MemoryBudget, CancellationToken, ValueRetentionError};

        for sign in [TemporalInfinity::Negative, TemporalInfinity::Positive] {
            for value in [
                sign.date(),
                sign.timestamp(),
                sign.timestamp_tz(),
                sign.interval(),
            ] {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let empty = MemoryBudget::new(0);
                let control = ProductionControl::new(&empty, &original, &invoking);
                let allocations = allocation_counter::measure(|| {
                    assert_eq!(
                        value
                            .parse_same_kind_at_with_control(sign.as_str(), 0, &control)
                            .unwrap(),
                        Some(value.clone())
                    );
                    assert!(matches!(
                        value.to_sql_string_with_control(&control),
                        Err(ValueRetentionError::Memory(_))
                    ));
                });
                assert_eq!(allocations.count_total, 0);
                assert_eq!(empty.used(), 0);

                let memory = MemoryBudget::new(64);
                let control = ProductionControl::new(&memory, &original, &invoking);
                let text = value.to_sql_string_with_control(&control).unwrap();
                assert_eq!(text.as_str(), sign.as_str());
                assert_eq!(memory.used(), text.capacity());
                drop(text);
                assert_eq!(memory.used(), 0);

                for cancel_original in [false, true] {
                    let original = CancellationToken::new();
                    let invoking = CancellationToken::new();
                    if cancel_original {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                    let control = ProductionControl::new(&memory, &original, &invoking);
                    assert!(matches!(
                        value.parse_same_kind_at_with_control(sign.as_str(), 0, &control),
                        Err(ValueRetentionError::Cancelled(_))
                    ));
                    assert!(matches!(
                        value.to_sql_string_with_control(&control),
                        Err(ValueRetentionError::Cancelled(_))
                    ));
                    assert_eq!(memory.used(), 0);
                }
            }
        }
    }
}
