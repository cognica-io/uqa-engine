//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! In-memory spatial index over geographic points.
//!
//! The current implementation uses a brute-force Haversine scan: simple,
//! correct, and sufficient for the algebraic operators that consume it.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{DocId, Payload, PostingEntry, PostingList};

const EARTH_RADIUS_M: f64 = 6_371_000.0;

/// Great-circle distance in meters between two `(longitude, latitude)`
/// pairs in degrees.
pub fn haversine_distance(lon1: f64, lat1: f64, lon2: f64, lat2: f64) -> f64 {
    let phi1 = lat1.to_radians();
    let phi2 = lat2.to_radians();
    let d_phi = (lat2 - lat1).to_radians();
    let d_lambda = (lon2 - lon1).to_radians();
    let a = (d_phi / 2.0).sin().powi(2) + phi1.cos() * phi2.cos() * (d_lambda / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    EARTH_RADIUS_M * c
}

pub trait SpatialIndex: Send + Sync {
    fn add(&mut self, doc_id: DocId, lon: f64, lat: f64);
    fn remove(&mut self, doc_id: DocId);
    fn clear(&mut self);
    fn search_within(&self, center_lon: f64, center_lat: f64, radius_m: f64) -> PostingList;
    fn count(&self) -> usize;
    fn snapshot(&self) -> Arc<dyn SpatialIndex>;
}

#[derive(Debug, Default, Clone)]
pub struct MemorySpatialIndex {
    field: String,
    points: BTreeMap<DocId, (f64, f64)>,
}

impl MemorySpatialIndex {
    pub fn new(field: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            points: BTreeMap::new(),
        }
    }

    pub fn field(&self) -> &str {
        &self.field
    }
}

impl SpatialIndex for MemorySpatialIndex {
    fn add(&mut self, doc_id: DocId, lon: f64, lat: f64) {
        self.points.insert(doc_id, (lon, lat));
    }

    fn remove(&mut self, doc_id: DocId) {
        self.points.remove(&doc_id);
    }

    fn clear(&mut self) {
        self.points.clear();
    }

    fn search_within(&self, center_lon: f64, center_lat: f64, radius_m: f64) -> PostingList {
        let mut entries: Vec<PostingEntry> = self
            .points
            .iter()
            .filter_map(|(&doc_id, &(lon, lat))| {
                let d = haversine_distance(center_lon, center_lat, lon, lat);
                if d <= radius_m {
                    let score = if radius_m > 0.0 {
                        1.0 - (d / radius_m)
                    } else {
                        1.0
                    };
                    Some(PostingEntry::new(doc_id, Payload::with_score(score)))
                } else {
                    None
                }
            })
            .collect();
        entries.sort_by_key(|e| e.doc_id);
        PostingList::from_sorted_unchecked(entries)
    }

    fn count(&self) -> usize {
        self.points.len()
    }

    fn snapshot(&self) -> Arc<dyn SpatialIndex> {
        Arc::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, eps: f64) {
        assert!((a - b).abs() < eps, "expected {a} ~ {b} within {eps}");
    }

    #[test]
    fn haversine_zero_for_identical_point() {
        approx(haversine_distance(0.0, 0.0, 0.0, 0.0), 0.0, 1e-6);
    }

    #[test]
    fn haversine_known_distance() {
        // London (-0.1276, 51.5074) to Paris (2.3522, 48.8566) ~= 343 km
        let d = haversine_distance(-0.1276, 51.5074, 2.3522, 48.8566);
        approx(d, 343_000.0, 5_000.0);
    }

    #[test]
    fn search_within_filters_by_distance() {
        let mut idx = MemorySpatialIndex::new("location");
        idx.add(1, 0.0, 0.0); // origin
        idx.add(2, 0.001, 0.0); // ~111 m east
        idx.add(3, 1.0, 1.0); // ~157 km away
        let pl = idx.search_within(0.0, 0.0, 500.0);
        let docs: Vec<DocId> = pl.iter().map(|e| e.doc_id).collect();
        assert_eq!(docs, vec![1, 2]);
        // Score is 1 - distance/radius, so doc 1 (distance 0) gets the
        // top score.
        let s1 = pl.get_entry(1).unwrap().payload.score;
        let s2 = pl.get_entry(2).unwrap().payload.score;
        assert!(s1 > s2);
    }
}
