// SPDX-License-Identifier: AGPL-3.0-only
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, RwLock},
};

use crate::{
    Document, DocumentId, Error, Query, QueryPlan, Result,
    index::IndexValue,
    query::{self, MAX_QUERY_RESULTS, SortDirection},
};

pub const MAX_ACTIVE_SNAPSHOTS: usize = 1_024;
pub const MAX_MVCC_HISTORY_BYTES: usize = 64 * 1024 * 1024;

/// Monotone commit ordinal. Document IDs and commit versions are separate types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitVersion(i64);

impl CommitVersion {
    /// Returns this version's positive transaction ordinal, or zero for an empty database.
    pub const fn value(self) -> i64 {
        self.0
    }

    pub(crate) const fn new(value: i64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone)]
struct DocumentVersion {
    version: CommitVersion,
    collection_id: i64,
    document: Option<Document>,
    encoded_bytes: usize,
}

#[derive(Debug)]
struct MvccState {
    current_version: CommitVersion,
    collections: HashMap<String, (i64, CommitVersion)>,
    documents: BTreeMap<i64, Vec<DocumentVersion>>,
    active_snapshots: BTreeMap<CommitVersion, usize>,
    history_bytes: usize,
}

impl Default for MvccState {
    fn default() -> Self {
        Self {
            current_version: CommitVersion::new(0),
            collections: HashMap::new(),
            documents: BTreeMap::new(),
            active_snapshots: BTreeMap::new(),
            history_bytes: 0,
        }
    }
}

pub(crate) struct MvccStore {
    state: RwLock<MvccState>,
}

impl Default for MvccStore {
    fn default() -> Self {
        Self {
            state: RwLock::new(MvccState::default()),
        }
    }
}

impl MvccStore {
    pub(crate) fn snapshot(self: &Arc<Self>) -> Result<Snapshot> {
        let mut state = self.state.write().map_err(|_| Error::LockPoisoned)?;
        let active_count = state.active_snapshots.values().sum::<usize>();
        if active_count >= MAX_ACTIVE_SNAPSHOTS {
            return Err(Error::Limit(format!(
                "active snapshot limit reached ({MAX_ACTIVE_SNAPSHOTS})"
            )));
        }
        let version = state.current_version;
        *state.active_snapshots.entry(version).or_default() += 1;
        drop(state);
        Ok(Snapshot {
            lease: Arc::new(SnapshotLease {
                store: Arc::clone(self),
                version,
            }),
        })
    }

    pub(crate) fn current_version(&self) -> Result<CommitVersion> {
        self.state
            .read()
            .map(|state| state.current_version)
            .map_err(|_| Error::LockPoisoned)
    }

    pub(crate) fn has_active_snapshots(&self) -> Result<bool> {
        self.state
            .read()
            .map(|state| !state.active_snapshots.is_empty())
            .map_err(|_| Error::LockPoisoned)
    }

    pub(crate) fn ensure_commit_capacity(
        &self,
        next_version: CommitVersion,
        changes: &[(i64, i64, Option<Document>)],
    ) -> Result<()> {
        let state = self.state.read().map_err(|_| Error::LockPoisoned)?;
        if next_version.value() <= 0
            || state.current_version.value().checked_add(1) != Some(next_version.value())
        {
            return Err(Error::TransactionState(
                "commit version is not the next monotone version".into(),
            ));
        }
        if state.active_snapshots.is_empty() {
            return Ok(());
        }

        let mut additional_history = 0_usize;
        for (document_id, _, _) in changes {
            let Some(previous) = state
                .documents
                .get(document_id)
                .and_then(|versions| versions.last())
            else {
                continue;
            };
            if previous.document.is_some()
                && state
                    .active_snapshots
                    .keys()
                    .any(|snapshot| *snapshot >= previous.version)
            {
                additional_history = additional_history
                    .checked_add(previous.encoded_bytes)
                    .ok_or_else(|| Error::Limit("MVCC history size overflow".into()))?;
            }
        }
        if state
            .history_bytes
            .checked_add(additional_history)
            .is_none_or(|bytes| bytes > MAX_MVCC_HISTORY_BYTES)
        {
            return Err(Error::Limit(format!(
                "commit would exceed the {MAX_MVCC_HISTORY_BYTES}-byte retained snapshot history limit"
            )));
        }
        Ok(())
    }

    pub(crate) fn apply_commit(
        &self,
        version: CommitVersion,
        new_collections: &[(String, i64)],
        changes: &[(i64, i64, Option<Document>)],
    ) -> Result<()> {
        let mut state = self.state.write().map_err(|_| Error::LockPoisoned)?;
        if state.current_version.value().checked_add(1) != Some(version.value()) {
            return Err(Error::Integrity(
                "MVCC commit version is not consecutive".into(),
            ));
        }
        let mut new_names = std::collections::HashSet::new();
        let mut new_ids = std::collections::HashSet::new();
        for (name, collection_id) in new_collections {
            if name.is_empty()
                || *collection_id <= 0
                || state.collections.contains_key(name)
                || state
                    .collections
                    .values()
                    .any(|(existing_id, _)| existing_id == collection_id)
                || !new_names.insert(name.as_str())
                || !new_ids.insert(*collection_id)
            {
                return Err(Error::Integrity(
                    "MVCC collection was created more than once".into(),
                ));
            }
        }
        let encoded_lengths: Vec<_> = changes
            .iter()
            .map(|(_, _, document)| {
                document
                    .as_ref()
                    .map(crate::codec::encode_document)
                    .transpose()
                    .map(|encoded| encoded.map_or(0, |bytes| bytes.len()))
            })
            .collect::<Result<_>>()?;
        for (name, collection_id) in new_collections {
            state
                .collections
                .insert(name.clone(), (*collection_id, version));
        }
        for ((document_id, collection_id, document), encoded_bytes) in
            changes.iter().zip(encoded_lengths)
        {
            let previous = state.documents.remove(document_id).unwrap_or_default();
            let previous_history_bytes = previous
                .iter()
                .take(previous.len().saturating_sub(1))
                .map(|entry| entry.encoded_bytes)
                .sum::<usize>();
            state.history_bytes = state.history_bytes.saturating_sub(previous_history_bytes);
            let mut versions = select_versions(&previous, &state.active_snapshots);
            versions.push(DocumentVersion {
                version,
                collection_id: *collection_id,
                document: document.clone(),
                encoded_bytes,
            });
            state.documents.insert(*document_id, versions);
        }
        state.current_version = version;
        prune_all(&mut state);
        Ok(())
    }

    pub(crate) fn reset(
        &self,
        version: CommitVersion,
        collections: &[(String, i64)],
        documents: &[(i64, i64, Document)],
    ) -> Result<()> {
        let mut state = self.state.write().map_err(|_| Error::LockPoisoned)?;
        if !state.active_snapshots.is_empty() {
            return Err(Error::ActiveSnapshots);
        }
        let mut next = MvccState {
            current_version: version,
            ..MvccState::default()
        };
        for (name, collection_id) in collections {
            next.collections
                .insert(name.clone(), (*collection_id, version));
        }
        for (document_id, collection_id, document) in documents {
            let encoded_bytes = crate::codec::encode_document(document)?.len();
            next.documents.insert(
                *document_id,
                vec![DocumentVersion {
                    version,
                    collection_id: *collection_id,
                    document: Some(document.clone()),
                    encoded_bytes,
                }],
            );
        }
        *state = next;
        Ok(())
    }
}

fn prune_all(state: &mut MvccState) {
    let snapshots = &state.active_snapshots;
    let mut total_history_bytes = 0_usize;
    state.documents.retain(|_, versions| {
        if versions.is_empty() {
            return false;
        }
        *versions = select_versions(versions, snapshots);
        if versions
            .last()
            .is_some_and(|entry| entry.document.is_none())
            && snapshots.is_empty()
        {
            return false;
        }
        if versions.len() > 1 {
            total_history_bytes = total_history_bytes.saturating_add(
                versions[..versions.len() - 1]
                    .iter()
                    .map(|entry| entry.encoded_bytes)
                    .sum::<usize>(),
            );
        }
        true
    });
    state.history_bytes = total_history_bytes;
}

fn select_versions(
    versions: &[DocumentVersion],
    snapshots: &BTreeMap<CommitVersion, usize>,
) -> Vec<DocumentVersion> {
    if versions.is_empty() {
        return Vec::new();
    }
    let mut selected = Vec::with_capacity(snapshots.len() + 1);
    for snapshot in snapshots.keys() {
        let index = versions.partition_point(|entry| entry.version <= *snapshot);
        if index > 0 {
            selected.push(index - 1);
        }
    }
    selected.push(versions.len() - 1);
    selected.sort_unstable();
    selected.dedup();
    selected
        .into_iter()
        .map(|index| versions[index].clone())
        .collect()
}

struct SnapshotLease {
    store: Arc<MvccStore>,
    version: CommitVersion,
}

impl Drop for SnapshotLease {
    fn drop(&mut self) {
        let mut state = self
            .store
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(count) = state.active_snapshots.get_mut(&self.version) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.active_snapshots.remove(&self.version);
            }
        }
        prune_all(&mut state);
    }
}

/// Stable read view at one commit version. Clones share a single snapshot lease.
#[derive(Clone)]
pub struct Snapshot {
    lease: Arc<SnapshotLease>,
}

/// Owned query row returned by an MVCC snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotRow {
    pub id: DocumentId,
    pub document: Document,
}

impl Snapshot {
    /// Returns the version observed by this snapshot.
    pub fn version(&self) -> CommitVersion {
        self.lease.version
    }

    /// Reads an owned document version visible to this snapshot.
    pub fn get(&self, collection: &str, id: DocumentId) -> Result<Option<Document>> {
        let state = self
            .lease
            .store
            .state
            .read()
            .map_err(|_| Error::LockPoisoned)?;
        let collection_id = collection_id_at(&state, collection, self.version())?;
        Ok(visible_document(&state, id.value(), collection_id, self.version()).cloned())
    }

    /// Lists IDs visible in this snapshot, in ascending ID order.
    pub fn document_ids(&self, collection: &str) -> Result<Vec<DocumentId>> {
        let state = self
            .lease
            .store
            .state
            .read()
            .map_err(|_| Error::LockPoisoned)?;
        let collection_id = collection_id_at(&state, collection, self.version())?;
        Ok(state
            .documents
            .keys()
            .filter_map(|id| {
                visible_document(&state, *id, collection_id, self.version())
                    .map(|_| DocumentId(*id))
            })
            .collect())
    }

    /// Returns a full-scan plan for a query over the versioned snapshot view.
    pub fn explain(&self, query: &Query) -> Result<QueryPlan> {
        validate_snapshot_query(query)?;
        let state = self
            .lease
            .store
            .state
            .read()
            .map_err(|_| Error::LockPoisoned)?;
        collection_id_at(&state, query.collection(), self.version())?;
        Ok(QueryPlan::FullScan {
            collection: query.collection().to_owned(),
        })
    }

    /// Executes a bounded native query against the stable snapshot view.
    pub fn execute(&self, query: &Query) -> Result<Vec<SnapshotRow>> {
        validate_snapshot_query(query)?;
        let state = self
            .lease
            .store
            .state
            .read()
            .map_err(|_| Error::LockPoisoned)?;
        let collection_id = collection_id_at(&state, query.collection(), self.version())?;
        let mut rows = Vec::new();
        for raw_id in state.documents.keys() {
            let Some(document) = visible_document(&state, *raw_id, collection_id, self.version())
            else {
                continue;
            };
            let id = DocumentId(*raw_id);
            if query::matches(query.filter(), id, document) {
                rows.push(SnapshotRow {
                    id,
                    document: document.clone(),
                });
            }
        }
        if let Some((path, direction)) = &query.sort {
            let mut keyed = rows
                .into_iter()
                .map(|row| {
                    let key = path
                        .get(&row.document)
                        .map(IndexValue::from_value)
                        .transpose()?;
                    Ok((key, row))
                })
                .collect::<Result<Vec<_>>>()?;
            keyed.sort_by(|(left_key, left), (right_key, right)| {
                let order = left_key.cmp(right_key);
                let order = if *direction == SortDirection::Descending {
                    order.reverse()
                } else {
                    order
                };
                if order == std::cmp::Ordering::Equal {
                    left.id.cmp(&right.id)
                } else {
                    order
                }
            });
            rows = keyed.into_iter().map(|(_, row)| row).collect();
        }
        let start = query.offset.min(rows.len());
        let end = start.saturating_add(query.limit).min(rows.len());
        Ok(rows.drain(start..end).collect())
    }
    /// Executes and projects a query against this fixed snapshot.
    pub fn execute_projected(
        &self,
        query: &Query,
        projection: &crate::Projection,
    ) -> Result<Vec<crate::ProjectedRow>> {
        let mut budget = crate::query::MAX_PROJECTED_BYTES;
        self
            .execute(query)?
            .into_iter()
            .map(|row| projection.apply_bounded(row.id, &row.document, &mut budget))
            .collect()
    }
}

fn collection_id_at(state: &MvccState, name: &str, version: CommitVersion) -> Result<i64> {
    state
        .collections
        .get(name)
        .filter(|(_, created)| *created <= version)
        .map(|(id, _)| *id)
        .ok_or_else(|| Error::NotFound(format!("collection {name:?}")))
}

fn visible_document(
    state: &MvccState,
    id: i64,
    collection_id: i64,
    version: CommitVersion,
) -> Option<&Document> {
    let versions = state.documents.get(&id)?;
    let index = versions.partition_point(|entry| entry.version <= version);
    if index == 0 {
        return None;
    }
    let entry = &versions[index - 1];
    (entry.collection_id == collection_id)
        .then_some(entry.document.as_ref())
        .flatten()
}

fn validate_snapshot_query(query: &Query) -> Result<()> {
    query::validate_filter(query.filter())?;
    crate::storage::validate_collection_name(query.collection())?;
    if query.limit > MAX_QUERY_RESULTS || query.offset > MAX_QUERY_RESULTS {
        return Err(Error::Limit(format!(
            "query limit and offset cannot exceed {MAX_QUERY_RESULTS}"
        )));
    }
    Ok(())
}
