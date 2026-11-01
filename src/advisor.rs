// SPDX-License-Identifier: AGPL-3.0-only
//! Local, opt-in workload observation for future engine planning.
//!
//! This module is deliberately not a public API or persistent database feature.
//! It observes a single embedded handle only. It never changes plans, indexes,
//! storage layout, transaction ordering, or database bytes.

use std::collections::BTreeMap;

use crate::{DocumentId, FieldPath, Filter, Query, QueryPlan};

/// A workload must produce this many observed full scans for one path before
/// the advisor suggests considering an access path. This is a recommendation,
/// not a cost model or an automatic DDL action.
pub(crate) const INDEX_RECOMMENDATION_SCAN_THRESHOLD: u64 = 8;
/// A document must receive this many committed writes before the advisor calls
/// it a hot-mutation candidate.
pub(crate) const HOT_DOCUMENT_WRITE_THRESHOLD: u64 = 16;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CollectionObservation {
    pub(crate) reads: u64,
    pub(crate) writes: u64,
    pub(crate) full_scans: u64,
    pub(crate) primary_key_lookups: u64,
    pub(crate) secondary_index_lookups: u64,
    pub(crate) secondary_index_range_scans: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OptimizationMetrics {
    pub(crate) enabled: bool,
    pub(crate) collections: BTreeMap<String, CollectionObservation>,
}

/// An explainable observation-derived suggestion. The advisor never applies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OptimizationRecommendation {
    ConsiderIndex {
        collection: String,
        path: FieldPath,
        observed_full_scans: u64,
    },
    PotentialHotDocument {
        collection: String,
        id: DocumentId,
        committed_writes: u64,
    },
}

#[derive(Default)]
pub(crate) struct OptimizationObserver {
    metrics: OptimizationMetrics,
    full_scan_paths: BTreeMap<(String, FieldPath), u64>,
    document_writes: BTreeMap<(String, DocumentId), u64>,
}

impl OptimizationObserver {
    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        self.metrics.enabled = enabled;
    }

    pub(crate) fn enabled(&self) -> bool {
        self.metrics.enabled
    }

    pub(crate) fn metrics(&self) -> OptimizationMetrics {
        self.metrics.clone()
    }

    pub(crate) fn record_query(&mut self, query: &Query, plan: &QueryPlan) {
        if !self.enabled() {
            return;
        }
        let collection = query.collection().to_owned();
        let metrics = self.metrics.collections.entry(collection.clone()).or_default();
        metrics.reads = metrics.reads.saturating_add(1);
        match plan {
            QueryPlan::PrimaryKeyLookup { .. } => {
                metrics.primary_key_lookups = metrics.primary_key_lookups.saturating_add(1);
            }
            QueryPlan::SecondaryIndexLookup { .. } => {
                metrics.secondary_index_lookups = metrics.secondary_index_lookups.saturating_add(1);
            }
            QueryPlan::SecondaryIndexRangeLookup { .. } => {
                metrics.secondary_index_range_scans =
                    metrics.secondary_index_range_scans.saturating_add(1);
            }
            QueryPlan::FullScan { .. } => {
                metrics.full_scans = metrics.full_scans.saturating_add(1);
                for path in comparison_paths(query.filter()) {
                    let count = self.full_scan_paths.entry((collection.clone(), path)).or_default();
                    *count = count.saturating_add(1);
                }
            }
        }
    }

    pub(crate) fn record_primary_read(&mut self, collection: &str) {
        if !self.enabled() {
            return;
        }
        let metrics = self
            .metrics
            .collections
            .entry(collection.to_owned())
            .or_default();
        metrics.reads = metrics.reads.saturating_add(1);
        metrics.primary_key_lookups = metrics.primary_key_lookups.saturating_add(1);
    }

    pub(crate) fn record_committed_write(&mut self, collection: String, id: DocumentId) {
        if !self.enabled() {
            return;
        }
        let metrics = self.metrics.collections.entry(collection.clone()).or_default();
        metrics.writes = metrics.writes.saturating_add(1);
        let count = self.document_writes.entry((collection, id)).or_default();
        *count = count.saturating_add(1);
    }

    pub(crate) fn recommendations(&self) -> Vec<OptimizationRecommendation> {
        if !self.enabled() {
            return Vec::new();
        }
        let mut recommendations = Vec::new();
        for ((collection, path), observed_full_scans) in &self.full_scan_paths {
            if *observed_full_scans >= INDEX_RECOMMENDATION_SCAN_THRESHOLD {
                recommendations.push(OptimizationRecommendation::ConsiderIndex {
                    collection: collection.clone(),
                    path: path.clone(),
                    observed_full_scans: *observed_full_scans,
                });
            }
        }
        for ((collection, id), committed_writes) in &self.document_writes {
            if *committed_writes >= HOT_DOCUMENT_WRITE_THRESHOLD {
                recommendations.push(OptimizationRecommendation::PotentialHotDocument {
                    collection: collection.clone(),
                    id: *id,
                    committed_writes: *committed_writes,
                });
            }
        }
        recommendations
    }
}

fn comparison_paths(filter: &Filter) -> Vec<FieldPath> {
    let mut paths = Vec::new();
    collect_comparison_paths(filter, &mut paths);
    paths.sort();
    paths.dedup();
    paths
}

fn collect_comparison_paths(filter: &Filter, paths: &mut Vec<FieldPath>) {
    match filter {
        Filter::Compare { path, .. } => paths.push(path.clone()),
        Filter::And(children) | Filter::Or(children) => {
            for child in children {
                collect_comparison_paths(child, paths);
            }
        }
        Filter::Not(child) => collect_comparison_paths(child, paths),
        Filter::All | Filter::IdEq(_) | Filter::Exists { .. } | Filter::ArrayContains { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use crate::{CompareOp, Filter, Query, QueryPlan, Value};

    use super::*;

    #[test]
    fn observation_is_opt_in_and_never_recommends_array_indexes() {
        let path = FieldPath::parse("account_id").unwrap();
        let query = Query::new(
            "transactions",
            Filter::And(vec![
                Filter::Compare {
                    path: path.clone(),
                    op: CompareOp::Eq,
                    value: Value::Integer(7),
                },
                Filter::ArrayContains {
                    path: FieldPath::parse("labels").unwrap(),
                    value: Value::String("work".into()),
                },
            ]),
        );
        let plan = QueryPlan::FullScan {
            collection: "transactions".into(),
        };
        let mut observer = OptimizationObserver::default();
        observer.record_query(&query, &plan);
        assert!(observer.metrics().collections.is_empty());

        observer.set_enabled(true);
        for _ in 0..INDEX_RECOMMENDATION_SCAN_THRESHOLD {
            observer.record_query(&query, &plan);
        }
        assert_eq!(
            observer.recommendations(),
            vec![OptimizationRecommendation::ConsiderIndex {
                collection: "transactions".into(),
                path,
                observed_full_scans: INDEX_RECOMMENDATION_SCAN_THRESHOLD,
            }]
        );
    }
}
