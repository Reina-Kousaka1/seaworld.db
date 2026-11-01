// SPDX-License-Identifier: AGPL-3.0-only
//! Local, opt-in workload observation for future engine planning.
//!
//! This module is a native diagnostic API, not a persistent database feature or
//! part of the transport-neutral `PublicEngine` contract. It observes a single
//! embedded handle only. It never changes plans, indexes, storage layout,
//! transaction ordering, or database bytes.

use std::collections::BTreeMap;

use crate::{DocumentId, FieldPath, Filter, Query, QueryPlan};

/// A workload must produce this many observed full scans for one path before
/// the advisor suggests considering an access path. This is a recommendation,
/// not a cost model or an automatic DDL action.
pub const INDEX_RECOMMENDATION_SCAN_THRESHOLD: u64 = 8;
/// A document must receive this many committed writes before the advisor calls
/// it a hot-mutation candidate.
pub const HOT_DOCUMENT_WRITE_THRESHOLD: u64 = 16;
/// A document must produce this many failed compare-and-set attempts before
/// the advisor calls it a contention candidate.
pub const CAS_CONTENTION_THRESHOLD: u64 = 8;

// Fine-grained candidates are deliberately capped. Aggregate collection
// counters continue after either cap is reached, but unbounded document/path
// cardinality must never turn opt-in diagnostics into a resource hazard.
const MAX_OBSERVED_SCAN_PATHS: usize = 1_024;
const MAX_OBSERVED_DOCUMENTS: usize = 1_024;

/// A qualitative estimate. SeaWorld deliberately does not manufacture timing,
/// storage, or throughput figures from handle-local diagnostic observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OptimizationLevel {
    Unknown,
    Low,
    Moderate,
    High,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CollectionObservation {
    pub reads: u64,
    pub writes: u64,
    pub full_scans: u64,
    pub primary_key_lookups: u64,
    pub secondary_index_lookups: u64,
    pub secondary_index_range_scans: u64,
    pub compare_and_set_attempts: u64,
    pub compare_and_set_failures: u64,
    pub committed_compare_and_sets: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct OptimizationMetrics {
    pub enabled: bool,
    pub collections: BTreeMap<String, CollectionObservation>,
    pub dropped_scan_path_candidates: u64,
    pub dropped_document_candidates: u64,
}

/// A bounded, owned summary of the observations available to the advisor.
///
/// It contains aggregate counters and candidate-map occupancy, not recorded
/// queries, query values, documents, or any persistent database state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct OptimizationWorkloadProfile {
    pub enabled: bool,
    pub collections: BTreeMap<String, CollectionObservation>,
    pub observed_scan_paths: usize,
    pub scan_path_capacity: usize,
    pub observed_documents: usize,
    pub document_capacity: usize,
    pub dropped_scan_path_candidates: u64,
    pub dropped_document_candidates: u64,
}

/// The kind of read-only optimization candidate derived from observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OptimizationCandidateKind {
    SecondaryAccessPath,
    HotDocumentWarning,
    CompareAndSetContentionWarning,
}

/// The logical target named by a candidate without exposing storage handles.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum OptimizationTarget {
    CollectionPath {
        collection: String,
        path: FieldPath,
    },
    Document {
        collection: String,
        id: DocumentId,
    },
}

/// Structured explanation for why a candidate exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OptimizationRationale {
    RepeatedFullScanPath,
    RepeatedCommittedDocumentWrites,
    RepeatedCompareAndSetFailures,
}

/// The observed counters that support one optimization candidate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct OptimizationEvidence {
    pub collection_reads: u64,
    pub collection_writes: u64,
    pub collection_full_scans: u64,
    pub matching_full_scans: u64,
    pub committed_document_writes: u64,
    pub compare_and_set_attempts: u64,
    pub compare_and_set_failures: u64,
}

/// A non-binding, explainable candidate; creating it never changes the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct OptimizationCandidate {
    pub kind: OptimizationCandidateKind,
    pub target: OptimizationTarget,
    pub rationale: OptimizationRationale,
    pub evidence: OptimizationEvidence,
    pub estimated_benefit: OptimizationLevel,
    pub estimated_cost: OptimizationLevel,
    pub confidence: OptimizationLevel,
}

/// A directional result of evaluating a candidate without applying it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OptimizationEffect {
    Unknown,
    NoExpectedChange,
    Reduced,
    Increased,
    Improved,
}

/// Read-only qualitative simulation of a candidate's possible effects.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct OptimizationSimulation {
    pub candidate: OptimizationCandidate,
    pub scan_pressure: OptimizationEffect,
    pub lookup_efficiency: OptimizationEffect,
    pub write_maintenance: OptimizationEffect,
    pub storage_footprint: OptimizationEffect,
}

/// An explainable observation-derived suggestion. The advisor never applies it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum OptimizationRecommendation {
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
    PotentialCompareAndSetContention {
        collection: String,
        id: DocumentId,
        observed_failures: u64,
    },
}

#[derive(Default)]
struct DocumentObservation {
    committed_writes: u64,
    compare_and_set_failures: u64,
}

#[derive(Default)]
pub(crate) struct OptimizationObserver {
    metrics: OptimizationMetrics,
    full_scan_paths: BTreeMap<(String, FieldPath), u64>,
    documents: BTreeMap<(String, DocumentId), DocumentObservation>,
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

    pub(crate) fn workload_profile(&self) -> OptimizationWorkloadProfile {
        OptimizationWorkloadProfile {
            enabled: self.enabled(),
            collections: self.metrics.collections.clone(),
            observed_scan_paths: self.full_scan_paths.len(),
            scan_path_capacity: MAX_OBSERVED_SCAN_PATHS,
            observed_documents: self.documents.len(),
            document_capacity: MAX_OBSERVED_DOCUMENTS,
            dropped_scan_path_candidates: self.metrics.dropped_scan_path_candidates,
            dropped_document_candidates: self.metrics.dropped_document_candidates,
        }
    }

    pub(crate) fn record_query(&mut self, query: &Query, plan: &QueryPlan) {
        if !self.enabled() {
            return;
        }
        let collection = query.collection().to_owned();
        let metrics = self
            .metrics
            .collections
            .entry(collection.clone())
            .or_default();
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
                    let key = (collection.clone(), path);
                    if let Some(count) = self.full_scan_paths.get_mut(&key) {
                        *count = count.saturating_add(1);
                    } else if self.full_scan_paths.len() < MAX_OBSERVED_SCAN_PATHS {
                        self.full_scan_paths.insert(key, 1);
                    } else {
                        self.metrics.dropped_scan_path_candidates =
                            self.metrics.dropped_scan_path_candidates.saturating_add(1);
                    }
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
        {
            let metrics = self
                .metrics
                .collections
                .entry(collection.clone())
                .or_default();
            metrics.writes = metrics.writes.saturating_add(1);
        }
        if let Some(observation) = self.document_observation(collection, id) {
            observation.committed_writes = observation.committed_writes.saturating_add(1);
        }
    }

    pub(crate) fn record_compare_and_set_attempt(
        &mut self,
        collection: String,
        id: DocumentId,
        condition_matched: bool,
    ) {
        if !self.enabled() {
            return;
        }
        {
            let metrics = self
                .metrics
                .collections
                .entry(collection.clone())
                .or_default();
            metrics.compare_and_set_attempts = metrics.compare_and_set_attempts.saturating_add(1);
            if !condition_matched {
                metrics.compare_and_set_failures =
                    metrics.compare_and_set_failures.saturating_add(1);
            }
        }
        if !condition_matched && let Some(observation) = self.document_observation(collection, id) {
            observation.compare_and_set_failures =
                observation.compare_and_set_failures.saturating_add(1);
        }
    }

    pub(crate) fn record_committed_compare_and_set(&mut self, collection: String) {
        if !self.enabled() {
            return;
        }
        let metrics = self.metrics.collections.entry(collection).or_default();
        metrics.committed_compare_and_sets = metrics.committed_compare_and_sets.saturating_add(1);
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
        for ((collection, id), observation) in &self.documents {
            if observation.committed_writes >= HOT_DOCUMENT_WRITE_THRESHOLD {
                recommendations.push(OptimizationRecommendation::PotentialHotDocument {
                    collection: collection.clone(),
                    id: *id,
                    committed_writes: observation.committed_writes,
                });
            }
            if observation.compare_and_set_failures >= CAS_CONTENTION_THRESHOLD {
                recommendations.push(
                    OptimizationRecommendation::PotentialCompareAndSetContention {
                        collection: collection.clone(),
                        id: *id,
                        observed_failures: observation.compare_and_set_failures,
                    },
                );
            }
        }
        recommendations
    }

    pub(crate) fn candidates(&self) -> Vec<OptimizationCandidate> {
        if !self.enabled() {
            return Vec::new();
        }
        let mut candidates = Vec::new();
        for ((collection, path), matching_full_scans) in &self.full_scan_paths {
            if *matching_full_scans < INDEX_RECOMMENDATION_SCAN_THRESHOLD {
                continue;
            }
            let metrics = self.collection_metrics(collection);
            candidates.push(OptimizationCandidate {
                kind: OptimizationCandidateKind::SecondaryAccessPath,
                target: OptimizationTarget::CollectionPath {
                    collection: collection.clone(),
                    path: path.clone(),
                },
                rationale: OptimizationRationale::RepeatedFullScanPath,
                evidence: OptimizationEvidence {
                    collection_reads: metrics.reads,
                    collection_writes: metrics.writes,
                    collection_full_scans: metrics.full_scans,
                    matching_full_scans: *matching_full_scans,
                    ..OptimizationEvidence::default()
                },
                estimated_benefit: scan_benefit(*matching_full_scans),
                estimated_cost: index_cost(metrics.writes, *matching_full_scans),
                confidence: confidence(*matching_full_scans, self.scan_candidates_complete()),
            });
        }
        for ((collection, id), observation) in &self.documents {
            let metrics = self.collection_metrics(collection);
            if observation.committed_writes >= HOT_DOCUMENT_WRITE_THRESHOLD {
                candidates.push(OptimizationCandidate {
                    kind: OptimizationCandidateKind::HotDocumentWarning,
                    target: OptimizationTarget::Document {
                        collection: collection.clone(),
                        id: *id,
                    },
                    rationale: OptimizationRationale::RepeatedCommittedDocumentWrites,
                    evidence: OptimizationEvidence {
                        collection_reads: metrics.reads,
                        collection_writes: metrics.writes,
                        collection_full_scans: metrics.full_scans,
                        committed_document_writes: observation.committed_writes,
                        ..OptimizationEvidence::default()
                    },
                    estimated_benefit: OptimizationLevel::Unknown,
                    estimated_cost: OptimizationLevel::Unknown,
                    confidence: confidence(
                        observation.committed_writes,
                        self.document_candidates_complete(),
                    ),
                });
            }
            if observation.compare_and_set_failures >= CAS_CONTENTION_THRESHOLD {
                candidates.push(OptimizationCandidate {
                    kind: OptimizationCandidateKind::CompareAndSetContentionWarning,
                    target: OptimizationTarget::Document {
                        collection: collection.clone(),
                        id: *id,
                    },
                    rationale: OptimizationRationale::RepeatedCompareAndSetFailures,
                    evidence: OptimizationEvidence {
                        collection_reads: metrics.reads,
                        collection_writes: metrics.writes,
                        collection_full_scans: metrics.full_scans,
                        compare_and_set_attempts: metrics.compare_and_set_attempts,
                        compare_and_set_failures: observation.compare_and_set_failures,
                        ..OptimizationEvidence::default()
                    },
                    estimated_benefit: OptimizationLevel::Unknown,
                    estimated_cost: OptimizationLevel::Unknown,
                    confidence: confidence(
                        observation.compare_and_set_failures,
                        self.document_candidates_complete(),
                    ),
                });
            }
        }
        candidates
    }

    pub(crate) fn simulations(&self) -> Vec<OptimizationSimulation> {
        self.candidates()
            .into_iter()
            .map(|candidate| {
                let index_candidate = matches!(
                    candidate.kind,
                    OptimizationCandidateKind::SecondaryAccessPath
                );
                OptimizationSimulation {
                    candidate,
                    scan_pressure: index_candidate
                        .then_some(OptimizationEffect::Reduced)
                        .unwrap_or(OptimizationEffect::Unknown),
                    lookup_efficiency: index_candidate
                        .then_some(OptimizationEffect::Improved)
                        .unwrap_or(OptimizationEffect::Unknown),
                    write_maintenance: index_candidate
                        .then_some(OptimizationEffect::Increased)
                        .unwrap_or(OptimizationEffect::Unknown),
                    storage_footprint: index_candidate
                        .then_some(OptimizationEffect::Increased)
                        .unwrap_or(OptimizationEffect::Unknown),
                }
            })
            .collect()
    }

    fn document_observation(
        &mut self,
        collection: String,
        id: DocumentId,
    ) -> Option<&mut DocumentObservation> {
        let key = (collection, id);
        if self.documents.contains_key(&key) {
            return self.documents.get_mut(&key);
        }
        if self.documents.len() >= MAX_OBSERVED_DOCUMENTS {
            self.metrics.dropped_document_candidates =
                self.metrics.dropped_document_candidates.saturating_add(1);
            return None;
        }
        self.documents
            .insert(key.clone(), DocumentObservation::default());
        self.documents.get_mut(&key)
    }

    fn collection_metrics(&self, collection: &str) -> CollectionObservation {
        self.metrics
            .collections
            .get(collection)
            .cloned()
            .unwrap_or_default()
    }

    fn scan_candidates_complete(&self) -> bool {
        self.metrics.dropped_scan_path_candidates == 0
    }

    fn document_candidates_complete(&self) -> bool {
        self.metrics.dropped_document_candidates == 0
    }
}

fn scan_benefit(observations: u64) -> OptimizationLevel {
    if observations >= INDEX_RECOMMENDATION_SCAN_THRESHOLD.saturating_mul(4) {
        OptimizationLevel::High
    } else {
        OptimizationLevel::Moderate
    }
}

fn index_cost(writes: u64, matching_full_scans: u64) -> OptimizationLevel {
    if writes == 0 {
        OptimizationLevel::Low
    } else if writes <= matching_full_scans {
        OptimizationLevel::Moderate
    } else {
        OptimizationLevel::High
    }
}

fn confidence(observations: u64, complete: bool) -> OptimizationLevel {
    if !complete {
        OptimizationLevel::Low
    } else if observations >= INDEX_RECOMMENDATION_SCAN_THRESHOLD.saturating_mul(4) {
        OptimizationLevel::High
    } else {
        OptimizationLevel::Moderate
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

    #[test]
    fn repeated_full_scans_produce_a_qualitative_read_only_simulation() {
        let path = FieldPath::parse("account_id").unwrap();
        let query = Query::new(
            "transactions",
            Filter::Compare {
                path: path.clone(),
                op: CompareOp::Eq,
                value: Value::Integer(7),
            },
        );
        let plan = QueryPlan::FullScan {
            collection: "transactions".into(),
        };
        let mut observer = OptimizationObserver::default();
        observer.set_enabled(true);
        for _ in 0..INDEX_RECOMMENDATION_SCAN_THRESHOLD {
            observer.record_query(&query, &plan);
        }

        let profile = observer.workload_profile();
        assert_eq!(profile.observed_scan_paths, 1);
        assert_eq!(profile.scan_path_capacity, MAX_OBSERVED_SCAN_PATHS);
        let candidates = observer.candidates();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].kind, OptimizationCandidateKind::SecondaryAccessPath);
        assert_eq!(
            candidates[0].target,
            OptimizationTarget::CollectionPath {
                collection: "transactions".into(),
                path,
            }
        );
        assert_eq!(
            candidates[0].rationale,
            OptimizationRationale::RepeatedFullScanPath
        );
        assert_eq!(candidates[0].evidence.matching_full_scans, 8);
        assert_eq!(candidates[0].estimated_benefit, OptimizationLevel::Moderate);
        assert_eq!(candidates[0].estimated_cost, OptimizationLevel::Low);
        assert_eq!(candidates[0].confidence, OptimizationLevel::Moderate);

        let simulations = observer.simulations();
        assert_eq!(simulations.len(), 1);
        assert_eq!(simulations[0].scan_pressure, OptimizationEffect::Reduced);
        assert_eq!(
            simulations[0].lookup_efficiency,
            OptimizationEffect::Improved
        );
        assert_eq!(
            simulations[0].write_maintenance,
            OptimizationEffect::Increased
        );
        assert_eq!(
            simulations[0].storage_footprint,
            OptimizationEffect::Increased
        );

        for _ in 0..INDEX_RECOMMENDATION_SCAN_THRESHOLD.saturating_mul(3) {
            observer.record_query(&query, &plan);
        }
        let well_observed = observer.candidates();
        assert_eq!(well_observed[0].estimated_benefit, OptimizationLevel::High);
        assert_eq!(well_observed[0].confidence, OptimizationLevel::High);
    }

    #[test]
    fn incomplete_scan_candidate_observation_lowers_confidence() {
        let target_path = FieldPath::parse("target").unwrap();
        let target = Query::new(
            "events",
            Filter::Compare {
                path: target_path.clone(),
                op: CompareOp::Eq,
                value: Value::Integer(1),
            },
        );
        let plan = QueryPlan::FullScan {
            collection: "events".into(),
        };
        let mut observer = OptimizationObserver::default();
        observer.set_enabled(true);
        for _ in 0..INDEX_RECOMMENDATION_SCAN_THRESHOLD {
            observer.record_query(&target, &plan);
        }
        for value in 0..=MAX_OBSERVED_SCAN_PATHS {
            let query = Query::new(
                "events",
                Filter::Compare {
                    path: FieldPath::parse(&format!("field_{value}")).unwrap(),
                    op: CompareOp::Eq,
                    value: Value::Integer(value as i64),
                },
            );
            observer.record_query(&query, &plan);
        }
        let candidate = observer
            .candidates()
            .into_iter()
            .find(|candidate| matches!(
                &candidate.target,
                OptimizationTarget::CollectionPath { collection, path }
                    if collection == "events" && path == &target_path
            ))
            .unwrap();
        assert_eq!(candidate.confidence, OptimizationLevel::Low);
        assert!(observer.workload_profile().dropped_scan_path_candidates > 0);
    }

    #[test]
    fn frequent_primary_reads_are_not_misclassified_as_document_contention() {
        let mut observer = OptimizationObserver::default();
        observer.set_enabled(true);
        for _ in 0..100 {
            observer.record_primary_read("accounts");
        }
        assert!(observer.candidates().is_empty());
        assert!(observer.simulations().is_empty());
    }

    #[test]
    fn contention_candidates_are_bounded_and_explainable() {
        let mut observer = OptimizationObserver::default();
        observer.set_enabled(true);
        let id = DocumentId::new(7).unwrap();
        for _ in 0..CAS_CONTENTION_THRESHOLD {
            observer.record_compare_and_set_attempt("accounts".into(), id, false);
        }
        observer.record_compare_and_set_attempt("accounts".into(), id, true);
        observer.record_committed_compare_and_set("accounts".into());

        let metrics = observer.metrics();
        assert_eq!(metrics.collections["accounts"].compare_and_set_attempts, 9);
        assert_eq!(metrics.collections["accounts"].compare_and_set_failures, 8);
        assert_eq!(
            metrics.collections["accounts"].committed_compare_and_sets,
            1
        );
        assert!(
            observer
                .recommendations()
                .iter()
                .any(|recommendation| matches!(
                    recommendation,
                    OptimizationRecommendation::PotentialCompareAndSetContention {
                        collection,
                        id: candidate,
                        observed_failures: CAS_CONTENTION_THRESHOLD,
                    } if collection == "accounts" && *candidate == id
                ))
        );

        for value in 1..=MAX_OBSERVED_DOCUMENTS + 1 {
            observer.record_committed_write("other".into(), DocumentId::new(value as i64).unwrap());
        }
        let profile = observer.workload_profile();
        assert_eq!(profile.observed_documents, MAX_OBSERVED_DOCUMENTS);
        assert_eq!(profile.document_capacity, MAX_OBSERVED_DOCUMENTS);
        assert!(profile.dropped_document_candidates > 0);
    }
}
