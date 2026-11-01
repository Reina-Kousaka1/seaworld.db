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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum OptimizationCandidateKind {
    SecondaryAccessPath,
    HotDocumentWarning,
    CompareAndSetContentionWarning,
}

/// The logical target named by a candidate without exposing storage handles.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum OptimizationTarget {
    CollectionPath { collection: String, path: FieldPath },
    Document { collection: String, id: DocumentId },
}

/// Structured explanation for why a candidate exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

/// The preceding and current bounded observation windows held by one handle.
///
/// Rotating a window retains only one preceding profile. Neither window is
/// persisted, and both are discarded when the database handle closes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct OptimizationObservationWindows {
    pub previous: Option<OptimizationWorkloadProfile>,
    pub current: OptimizationWorkloadProfile,
}

/// A workload dimension whose observed share changed between two windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkloadDriftDimension {
    FullScanShare,
    PrimaryKeyLookupShare,
    RangeLookupShare,
    WriteShare,
}

/// The direction of one explainable workload-share change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkloadDriftDirection {
    Increased,
    Decreased,
}

/// Count-based evidence for one workload-share change.
///
/// Consumers can calculate a ratio if they need one. SeaWorld deliberately
/// reports the observed numerators and denominators instead of a fabricated
/// performance estimate.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WorkloadDriftEvidence {
    pub dimension: WorkloadDriftDimension,
    pub direction: WorkloadDriftDirection,
    pub previous_matching_operations: u64,
    pub previous_total_operations: u64,
    pub current_matching_operations: u64,
    pub current_total_operations: u64,
}

/// A deterministic, qualitative comparison of the previous and current
/// bounded workload windows.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct OptimizationWorkloadDrift {
    pub severity: OptimizationLevel,
    pub confidence: OptimizationLevel,
    pub evidence: Vec<WorkloadDriftEvidence>,
}

/// The diagnostic lifecycle state of a recommendation or simulation.
///
/// These states never cause an optimization to be applied. `Observed` means
/// a former recommendation lacks enough current-window evidence to be
/// reclassified. `Recommended` and `Simulated` describe current diagnostics;
/// `Stale` and `Invalidated` describe old evidence after material drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OptimizationCandidateLifecycleState {
    Observed,
    Recommended,
    Simulated,
    Stale,
    Invalidated,
}

/// A candidate evaluated against the current window after a rotation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct OptimizationCandidateLifecycle {
    pub candidate: OptimizationCandidate,
    pub current_candidate: Option<OptimizationCandidate>,
    pub state: OptimizationCandidateLifecycleState,
    pub baseline_confidence: Option<OptimizationLevel>,
    pub effective_confidence: OptimizationLevel,
    pub drift: Option<OptimizationWorkloadDrift>,
}

/// Re-evaluation of a simulation captured from the preceding window.
///
/// A `Stale` or `Invalidated` result means its former directional estimate
/// must not be presented as a current-workload estimate.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct OptimizationSimulationReevaluation {
    pub previous: OptimizationSimulation,
    pub current: Option<OptimizationSimulation>,
    pub state: OptimizationCandidateLifecycleState,
    pub effective_confidence: OptimizationLevel,
    pub drift: Option<OptimizationWorkloadDrift>,
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
    previous_window: Option<ObservationWindow>,
}

#[derive(Clone)]
struct ObservationWindow {
    profile: OptimizationWorkloadProfile,
    candidates: Vec<OptimizationCandidate>,
    simulations: Vec<OptimizationSimulation>,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CandidateIdentity {
    kind: OptimizationCandidateKind,
    target: OptimizationTarget,
    rationale: OptimizationRationale,
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

    pub(crate) fn workload_windows(&self) -> OptimizationObservationWindows {
        OptimizationObservationWindows {
            previous: self
                .previous_window
                .as_ref()
                .map(|window| window.profile.clone()),
            current: self.workload_profile(),
        }
    }

    /// Finishes the current diagnostic window and starts a fresh one.
    ///
    /// This affects only opt-in, handle-local diagnostic memory. It never
    /// changes the observed database, its query plans, or transaction state.
    pub(crate) fn rotate_workload_window(&mut self) -> OptimizationObservationWindows {
        if !self.enabled() {
            return self.workload_windows();
        }

        let profile = self.workload_profile();
        let candidates = self.candidates();
        let simulations = self.simulations();
        self.previous_window = Some(ObservationWindow {
            profile,
            candidates,
            simulations,
        });
        self.metrics = OptimizationMetrics {
            enabled: true,
            ..OptimizationMetrics::default()
        };
        self.full_scan_paths.clear();
        self.documents.clear();
        self.workload_windows()
    }

    pub(crate) fn workload_drift(&self) -> Option<OptimizationWorkloadDrift> {
        let previous = self.previous_window.as_ref()?;
        workload_drift(&previous.profile, &self.workload_profile())
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
                    scan_pressure: if index_candidate {
                        OptimizationEffect::Reduced
                    } else {
                        OptimizationEffect::Unknown
                    },
                    lookup_efficiency: if index_candidate {
                        OptimizationEffect::Improved
                    } else {
                        OptimizationEffect::Unknown
                    },
                    write_maintenance: if index_candidate {
                        OptimizationEffect::Increased
                    } else {
                        OptimizationEffect::Unknown
                    },
                    storage_footprint: if index_candidate {
                        OptimizationEffect::Increased
                    } else {
                        OptimizationEffect::Unknown
                    },
                }
            })
            .collect()
    }

    pub(crate) fn candidate_lifecycle(&self) -> Vec<OptimizationCandidateLifecycle> {
        let current: BTreeMap<_, _> = self
            .candidates()
            .into_iter()
            .map(|candidate| (candidate_identity(&candidate), candidate))
            .collect();
        let Some(previous) = self.previous_window.as_ref() else {
            return current
                .into_values()
                .map(|candidate| OptimizationCandidateLifecycle {
                    effective_confidence: candidate.confidence,
                    candidate,
                    current_candidate: None,
                    state: OptimizationCandidateLifecycleState::Recommended,
                    baseline_confidence: None,
                    drift: None,
                })
                .collect();
        };

        let drift = self.workload_drift();
        let material_drift = is_material_drift(drift.as_ref());
        let mild_drift = is_mild_drift(drift.as_ref());
        let mut remaining = current;
        let mut lifecycle = Vec::new();

        for previous_candidate in &previous.candidates {
            let current_candidate = remaining.remove(&candidate_identity(previous_candidate));
            let candidate = current_candidate
                .clone()
                .unwrap_or_else(|| previous_candidate.clone());
            let state = match (&current_candidate, material_drift) {
                (Some(_), true) => OptimizationCandidateLifecycleState::Stale,
                (None, true) => OptimizationCandidateLifecycleState::Invalidated,
                (Some(_), false) => OptimizationCandidateLifecycleState::Recommended,
                (None, false) => OptimizationCandidateLifecycleState::Observed,
            };
            let effective_confidence =
                lifecycle_confidence(candidate.confidence, material_drift, mild_drift);
            lifecycle.push(OptimizationCandidateLifecycle {
                candidate,
                current_candidate,
                state,
                baseline_confidence: Some(previous_candidate.confidence),
                effective_confidence,
                drift: drift.clone(),
            });
        }
        lifecycle.extend(
            remaining
                .into_values()
                .map(|candidate| OptimizationCandidateLifecycle {
                    effective_confidence: candidate.confidence,
                    candidate,
                    current_candidate: None,
                    state: OptimizationCandidateLifecycleState::Recommended,
                    baseline_confidence: None,
                    drift: drift.clone(),
                }),
        );
        lifecycle
    }

    pub(crate) fn simulation_reevaluations(&self) -> Vec<OptimizationSimulationReevaluation> {
        let Some(previous) = self.previous_window.as_ref() else {
            return Vec::new();
        };
        let current: BTreeMap<_, _> = self
            .simulations()
            .into_iter()
            .map(|simulation| (candidate_identity(&simulation.candidate), simulation))
            .collect();
        let drift = self.workload_drift();
        let material_drift = is_material_drift(drift.as_ref());
        let mild_drift = is_mild_drift(drift.as_ref());
        previous
            .simulations
            .iter()
            .map(|previous_simulation| {
                let current_simulation = current
                    .get(&candidate_identity(&previous_simulation.candidate))
                    .cloned();
                let state = match (&current_simulation, material_drift) {
                    (Some(_), true) => OptimizationCandidateLifecycleState::Stale,
                    (None, true) => OptimizationCandidateLifecycleState::Invalidated,
                    (Some(_), false) => OptimizationCandidateLifecycleState::Simulated,
                    (None, false) => OptimizationCandidateLifecycleState::Observed,
                };
                OptimizationSimulationReevaluation {
                    effective_confidence: lifecycle_confidence(
                        previous_simulation.candidate.confidence,
                        material_drift,
                        mild_drift,
                    ),
                    previous: previous_simulation.clone(),
                    current: current_simulation,
                    state,
                    drift: drift.clone(),
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

fn candidate_identity(candidate: &OptimizationCandidate) -> CandidateIdentity {
    CandidateIdentity {
        kind: candidate.kind,
        target: candidate.target.clone(),
        rationale: candidate.rationale,
    }
}

fn is_material_drift(drift: Option<&OptimizationWorkloadDrift>) -> bool {
    matches!(
        drift.map(|drift| drift.severity),
        Some(OptimizationLevel::Moderate | OptimizationLevel::High)
    )
}

fn is_mild_drift(drift: Option<&OptimizationWorkloadDrift>) -> bool {
    matches!(
        drift.map(|drift| drift.severity),
        Some(OptimizationLevel::Low)
    )
}

fn lifecycle_confidence(
    confidence: OptimizationLevel,
    material_drift: bool,
    mild_drift: bool,
) -> OptimizationLevel {
    if material_drift {
        OptimizationLevel::Low
    } else if mild_drift && confidence == OptimizationLevel::High {
        OptimizationLevel::Moderate
    } else {
        confidence
    }
}

#[derive(Default)]
struct WorkloadMix {
    reads: u64,
    writes: u64,
    full_scans: u64,
    primary_key_lookups: u64,
    range_lookups: u64,
}

struct WorkloadShare {
    matching: u64,
    total: u64,
}

fn workload_drift(
    previous: &OptimizationWorkloadProfile,
    current: &OptimizationWorkloadProfile,
) -> Option<OptimizationWorkloadDrift> {
    let previous = workload_mix(previous);
    let current = workload_mix(current);
    let mut evidence = Vec::new();
    let mut largest_change = 0;

    compare_share(
        &mut evidence,
        &mut largest_change,
        WorkloadDriftDimension::FullScanShare,
        WorkloadShare {
            matching: previous.full_scans,
            total: previous.reads,
        },
        WorkloadShare {
            matching: current.full_scans,
            total: current.reads,
        },
    );
    compare_share(
        &mut evidence,
        &mut largest_change,
        WorkloadDriftDimension::PrimaryKeyLookupShare,
        WorkloadShare {
            matching: previous.primary_key_lookups,
            total: previous.reads,
        },
        WorkloadShare {
            matching: current.primary_key_lookups,
            total: current.reads,
        },
    );
    compare_share(
        &mut evidence,
        &mut largest_change,
        WorkloadDriftDimension::RangeLookupShare,
        WorkloadShare {
            matching: previous.range_lookups,
            total: previous.reads,
        },
        WorkloadShare {
            matching: current.range_lookups,
            total: current.reads,
        },
    );
    compare_share(
        &mut evidence,
        &mut largest_change,
        WorkloadDriftDimension::WriteShare,
        WorkloadShare {
            matching: previous.writes,
            total: previous.reads.saturating_add(previous.writes),
        },
        WorkloadShare {
            matching: current.writes,
            total: current.reads.saturating_add(current.writes),
        },
    );

    if evidence.is_empty() {
        return None;
    }
    let observations = previous
        .reads
        .saturating_add(previous.writes)
        .min(current.reads.saturating_add(current.writes));
    Some(OptimizationWorkloadDrift {
        severity: if largest_change >= 50 {
            OptimizationLevel::High
        } else if largest_change >= 25 {
            OptimizationLevel::Moderate
        } else {
            OptimizationLevel::Low
        },
        confidence: if observations < INDEX_RECOMMENDATION_SCAN_THRESHOLD {
            OptimizationLevel::Low
        } else if observations < INDEX_RECOMMENDATION_SCAN_THRESHOLD.saturating_mul(4) {
            OptimizationLevel::Moderate
        } else {
            OptimizationLevel::High
        },
        evidence,
    })
}

fn workload_mix(profile: &OptimizationWorkloadProfile) -> WorkloadMix {
    profile
        .collections
        .values()
        .fold(WorkloadMix::default(), |mut mix, metrics| {
            mix.reads = mix.reads.saturating_add(metrics.reads);
            mix.writes = mix.writes.saturating_add(metrics.writes);
            mix.full_scans = mix.full_scans.saturating_add(metrics.full_scans);
            mix.primary_key_lookups = mix
                .primary_key_lookups
                .saturating_add(metrics.primary_key_lookups);
            mix.range_lookups = mix
                .range_lookups
                .saturating_add(metrics.secondary_index_range_scans);
            mix
        })
}

fn compare_share(
    evidence: &mut Vec<WorkloadDriftEvidence>,
    largest_change: &mut u64,
    dimension: WorkloadDriftDimension,
    previous: WorkloadShare,
    current: WorkloadShare,
) {
    if previous.total == 0 || current.total == 0 {
        return;
    }
    let previous_share = previous.matching.saturating_mul(100) / previous.total;
    let current_share = current.matching.saturating_mul(100) / current.total;
    let change = previous_share.abs_diff(current_share);
    if change < 10 {
        return;
    }
    *largest_change = (*largest_change).max(change);
    evidence.push(WorkloadDriftEvidence {
        dimension,
        direction: if current_share > previous_share {
            WorkloadDriftDirection::Increased
        } else {
            WorkloadDriftDirection::Decreased
        },
        previous_matching_operations: previous.matching,
        previous_total_operations: previous.total,
        current_matching_operations: current.matching,
        current_total_operations: current.total,
    });
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

    fn full_scan_query() -> (Query, QueryPlan) {
        (
            Query::new(
                "events",
                Filter::Compare {
                    path: FieldPath::parse("account").unwrap(),
                    op: CompareOp::Eq,
                    value: Value::Integer(7),
                },
            ),
            QueryPlan::FullScan {
                collection: "events".into(),
            },
        )
    }

    fn record_full_scans(observer: &mut OptimizationObserver, count: u64) {
        let (query, plan) = full_scan_query();
        for _ in 0..count {
            observer.record_query(&query, &plan);
        }
    }

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
        assert_eq!(
            candidates[0].kind,
            OptimizationCandidateKind::SecondaryAccessPath
        );
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
            .find(|candidate| {
                matches!(
                    &candidate.target,
                    OptimizationTarget::CollectionPath { collection, path }
                        if collection == "events" && path == &target_path
                )
            })
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

    #[test]
    fn similar_windows_do_not_report_drift() {
        let mut observer = OptimizationObserver::default();
        observer.set_enabled(true);
        for _ in 0..32 {
            observer.record_primary_read("events");
        }
        let windows = observer.rotate_workload_window();
        assert_eq!(windows.previous.unwrap().collections["events"].reads, 32);
        assert!(windows.current.collections.is_empty());

        for _ in 0..32 {
            observer.record_primary_read("events");
        }
        assert!(observer.workload_drift().is_none());
    }

    #[test]
    fn drift_is_explainable_at_low_moderate_and_high_severity() {
        let scenarios = [
            (4_u64, OptimizationLevel::Low),
            (8_u64, OptimizationLevel::Moderate),
            (16_u64, OptimizationLevel::High),
        ];
        for (full_scans, expected_severity) in scenarios {
            let mut observer = OptimizationObserver::default();
            observer.set_enabled(true);
            for _ in 0..32 {
                observer.record_primary_read("events");
            }
            observer.rotate_workload_window();
            record_full_scans(&mut observer, full_scans);
            for _ in 0..32 - full_scans {
                observer.record_primary_read("events");
            }

            let drift = observer.workload_drift().unwrap();
            assert_eq!(drift.severity, expected_severity);
            assert_eq!(drift.confidence, OptimizationLevel::High);
            assert!(drift.evidence.iter().any(|evidence| {
                evidence.dimension == WorkloadDriftDimension::FullScanShare
                    && evidence.direction == WorkloadDriftDirection::Increased
            }));
        }
    }

    #[test]
    fn small_windows_report_low_drift_confidence() {
        let mut observer = OptimizationObserver::default();
        observer.set_enabled(true);
        for _ in 0..4 {
            observer.record_primary_read("events");
        }
        observer.rotate_workload_window();
        record_full_scans(&mut observer, 4);

        let drift = observer.workload_drift().unwrap();
        assert_eq!(drift.severity, OptimizationLevel::High);
        assert_eq!(drift.confidence, OptimizationLevel::Low);
    }

    #[test]
    fn stable_candidate_evidence_remains_recommended_and_simulated() {
        let mut observer = OptimizationObserver::default();
        observer.set_enabled(true);
        record_full_scans(&mut observer, INDEX_RECOMMENDATION_SCAN_THRESHOLD);
        observer.rotate_workload_window();
        record_full_scans(&mut observer, INDEX_RECOMMENDATION_SCAN_THRESHOLD);

        assert!(observer.workload_drift().is_none());
        assert_eq!(
            observer.candidate_lifecycle()[0].state,
            OptimizationCandidateLifecycleState::Recommended
        );
        assert_eq!(
            observer.simulation_reevaluations()[0].state,
            OptimizationCandidateLifecycleState::Simulated
        );
    }

    #[test]
    fn low_drift_only_decays_high_confidence_to_moderate() {
        assert_eq!(
            lifecycle_confidence(OptimizationLevel::High, false, true),
            OptimizationLevel::Moderate
        );
    }

    #[test]
    fn rotation_keeps_only_one_bounded_previous_window() {
        let mut observer = OptimizationObserver::default();
        observer.set_enabled(true);
        let (query, plan) = full_scan_query();
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
        let windows = observer.rotate_workload_window();
        let previous = windows.previous.unwrap();
        assert_eq!(previous.observed_scan_paths, MAX_OBSERVED_SCAN_PATHS);
        assert!(previous.dropped_scan_path_candidates > 0);
        assert!(windows.current.collections.is_empty());

        observer.record_query(&query, &plan);
        let windows = observer.rotate_workload_window();
        assert_eq!(windows.previous.unwrap().observed_scan_paths, 1);
        assert!(windows.current.collections.is_empty());
    }

    #[test]
    fn material_drift_marks_candidates_and_simulations_stale() {
        let mut observer = OptimizationObserver::default();
        observer.set_enabled(true);
        record_full_scans(&mut observer, INDEX_RECOMMENDATION_SCAN_THRESHOLD);
        observer.rotate_workload_window();

        record_full_scans(&mut observer, INDEX_RECOMMENDATION_SCAN_THRESHOLD);
        for _ in 0..24 {
            observer.record_primary_read("events");
        }

        let lifecycle = observer.candidate_lifecycle();
        assert_eq!(lifecycle.len(), 1);
        assert_eq!(
            lifecycle[0].state,
            OptimizationCandidateLifecycleState::Stale
        );
        assert_eq!(lifecycle[0].effective_confidence, OptimizationLevel::Low);
        assert!(lifecycle[0].current_candidate.is_some());

        let simulations = observer.simulation_reevaluations();
        assert_eq!(simulations.len(), 1);
        assert_eq!(
            simulations[0].state,
            OptimizationCandidateLifecycleState::Stale
        );
        assert!(simulations[0].current.is_some());
    }

    #[test]
    fn material_drift_invalidates_unrepresented_prior_candidate() {
        let mut observer = OptimizationObserver::default();
        observer.set_enabled(true);
        record_full_scans(&mut observer, INDEX_RECOMMENDATION_SCAN_THRESHOLD);
        observer.rotate_workload_window();
        for _ in 0..32 {
            observer.record_primary_read("events");
        }

        let lifecycle = observer.candidate_lifecycle();
        assert_eq!(lifecycle.len(), 1);
        assert_eq!(
            lifecycle[0].state,
            OptimizationCandidateLifecycleState::Invalidated
        );
        assert_eq!(lifecycle[0].effective_confidence, OptimizationLevel::Low);
        let simulations = observer.simulation_reevaluations();
        assert_eq!(simulations.len(), 1);
        assert_eq!(
            simulations[0].state,
            OptimizationCandidateLifecycleState::Invalidated
        );
    }
}
