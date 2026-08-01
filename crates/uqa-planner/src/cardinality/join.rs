//! Generic independent-domain join estimate.

use super::CardinalityEstimator;

impl CardinalityEstimator {
    /// Uniform, independent-key join heuristic `|L1| * |L2| / |dom(f)|`.
    ///
    /// The formula is a cost estimate under those assumptions, not a bound on
    /// the realized join cardinality.
    pub fn estimate_join(&self, left_card: f64, right_card: f64, domain_size: f64) -> f64 {
        if domain_size <= 0.0 {
            return 0.0;
        }
        (left_card * right_card) / domain_size
    }
}
