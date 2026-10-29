// SPDX-License-Identifier: AGPL-3.0-only
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::{self, File, OpenOptions, TryLockError},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
};

use crate::{
    CollectionId, Document, DocumentId, Error, FieldPath, IndexId, Query, QueryPlan, QueryRow,
    Result, Value,
    btree::{self, TreeKey},
    codec::{self, Cursor},
    format::{
        self, FORMAT_VERSION, HEADER_SIZE, MAX_RECORD_SIZE, MIN_RECORD_SIZE, MVCC_FORMAT_VERSION,
        PERSISTENT_INDEX_FORMAT_VERSION, RECORD_MAGIC, RECORD_PREFIX_SIZE,
        TRANSACTION_FORMAT_VERSION,
    },
    index::{IndexValue, MAX_INDEXES, SecondaryIndex},
    mvcc::{CommitVersion, MvccStore, Snapshot},
    query::{self, CompareOp, Filter, MAX_QUERY_RESULTS, SortDirection},
};

/// Maximum supported file size for current SeaWorld database formats.
pub const MAX_DATABASE_BYTES: u64 = format::MAX_DATABASE_SIZE_BYTES;
/// Maximum active collections in one database.
pub const MAX_COLLECTIONS: usize = 10_000;
/// Maximum active documents in one database.
pub const MAX_DOCUMENTS: usize = 100_000;
/// Maximum mutations permitted in one V2/V3 transaction.
pub const MAX_TRANSACTION_OPERATIONS: usize = 10_000;
/// Maximum encoded mutation payload bytes in one V2/V3 transaction.
pub const MAX_TRANSACTION_BYTES: usize = 64 * 1024 * 1024;

const COLLECTION_NAME_MAX_BYTES: usize = 255;
static NEXT_COMPACTION_TEMP_ID: AtomicU64 = AtomicU64::new(1);

/// Number of bytes discarded from an incomplete record or uncommitted V2 suffix.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    discarded_bytes: u64,
}

impl RecoveryReport {
    /// Returns the number of bytes removed from an incomplete final record.
    pub const fn discarded_bytes(self) -> u64 {
        self.discarded_bytes
    }
}

/// Read-only summary returned after validating the currently opened database view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegrityReport {
    pub format_version: u16,
    pub database_id: [u8; 16],
    pub file_bytes: u64,
    pub collection_count: usize,
    pub document_count: usize,
    pub index_count: usize,
    pub persistent_page_count: usize,
    pub committed_version: i64,
    pub discarded_tail_bytes: u64,
}

/// Public metadata for one persisted single-field index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInfo {
    pub id: IndexId,
    pub collection: String,
    pub path: FieldPath,
    pub unique: bool,
    pub root_page_id: Option<u64>,
    pub root_generation: Option<u64>,
}

struct CollectionState {
    id: i64,
    documents: BTreeMap<i64, Document>,
}

/// Embedded database handle backed by a versioned SeaWorld append-only file.
///
/// A writer handle owns an exclusive sidecar lock. Each file append is protected
/// by an OS lock on the database file and is durable after its checksummed bytes
/// have been synchronized successfully.
pub struct Database {
    path: PathBuf,
    file: File,
    writer_lock: Option<File>,
    writable: bool,
    database_id: [u8; 16],
    format_version: u16,
    collections: HashMap<String, CollectionState>,
    collection_names_by_id: HashMap<i64, String>,
    primary_id_index: HashMap<i64, i64>,
    secondary_indexes: BTreeMap<i64, SecondaryIndex>,
    persistent_pages: HashMap<u64, Vec<u8>>,
    next_page_id: u64,
    next_sequence: i64,
    next_collection_id: i64,
    next_document_id: i64,
    next_transaction_id: i64,
    next_index_id: i64,
    document_ids_exhausted: bool,
    document_count: usize,
    write_failed: bool,
    recovery_report: RecoveryReport,
    mvcc: Arc<MvccStore>,
}

impl Database {
    /// Creates a new database file and never overwrites an existing path.
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        Self::create_with_version(path.as_ref(), PERSISTENT_INDEX_FORMAT_VERSION)
    }

    /// Creates an explicit legacy V3 database for compatibility tooling.
    pub fn create_v3(path: impl AsRef<Path>) -> Result<Self> {
        Self::create_with_version(path.as_ref(), MVCC_FORMAT_VERSION)
    }

    /// Creates an explicit legacy V2 database for compatibility tooling.
    pub fn create_v2(path: impl AsRef<Path>) -> Result<Self> {
        Self::create_with_version(path.as_ref(), TRANSACTION_FORMAT_VERSION)
    }

    /// Creates an explicit legacy V1 database for compatibility tooling.
    pub fn create_v1(path: impl AsRef<Path>) -> Result<Self> {
        Self::create_with_version(path.as_ref(), FORMAT_VERSION)
    }

    fn create_with_version(path: &Path, format_version: u16) -> Result<Self> {
        Self::create_with_identity(path, random_database_id()?, format_version)
    }

    fn create_with_identity(
        path: &Path,
        database_id: [u8; 16],
        format_version: u16,
    ) -> Result<Self> {
        let path = absolute_path(path)?;
        let writer_lock = acquire_writer_lock(&path)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.lock()?;

        let header = format::encode_header_version(database_id, format_version);
        file.write_all(&header)?;
        file.sync_all()?;
        sync_parent_directory(&path)?;
        file.unlock()?;

        Ok(Self::new(
            path,
            file,
            Some(writer_lock),
            true,
            database_id,
            format_version,
        ))
    }

    /// Opens a database for writing, validates the file, and replays complete records.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = absolute_path(path.as_ref())?;
        let writer_lock = acquire_writer_lock(&path)?;
        let mut file = OpenOptions::new().read(true).write(true).open(&path)?;
        file.lock()?;
        let file_size = file.metadata()?.len();
        if file_size > MAX_DATABASE_BYTES {
            return Err(Error::Format(
                "database file exceeds the 128 MiB limit".into(),
            ));
        }
        if file_size < HEADER_SIZE as u64 {
            return Err(Error::Format(
                "database file is shorter than its header".into(),
            ));
        }

        let mut header = [0_u8; HEADER_SIZE];
        file.read_exact(&mut header)?;
        let decoded = format::decode_header(&header)?;
        let mut database = Self::new(
            path,
            file,
            Some(writer_lock),
            true,
            decoded.database_id,
            decoded.version,
        );
        database.replay_records(file_size)?;
        database.reset_mvcc_to_current()?;
        database.file.unlock()?;
        Ok(database)
    }

    /// Opens a fixed read-only view while a shared file lock prevents replay during an append.
    pub fn open_reader(path: impl AsRef<Path>) -> Result<Self> {
        let path = absolute_path(path.as_ref())?;
        let mut file = File::open(&path)?;
        file.lock_shared()?;
        let file_size = file.metadata()?.len();
        if file_size > MAX_DATABASE_BYTES {
            return Err(Error::Format(
                "database file exceeds the 128 MiB limit".into(),
            ));
        }
        if file_size < HEADER_SIZE as u64 {
            return Err(Error::Format(
                "database file is shorter than its header".into(),
            ));
        }
        let mut header = [0_u8; HEADER_SIZE];
        file.read_exact(&mut header)?;
        let decoded = format::decode_header(&header)?;
        let mut database = Self::new(
            path,
            file,
            None,
            false,
            decoded.database_id,
            decoded.version,
        );
        database.replay_records(file_size)?;
        database.reset_mvcc_to_current()?;
        database.file.unlock()?;
        Ok(database)
    }

    fn new(
        path: PathBuf,
        file: File,
        writer_lock: Option<File>,
        writable: bool,
        database_id: [u8; 16],
        format_version: u16,
    ) -> Self {
        Self {
            path,
            file,
            writer_lock,
            writable,
            database_id,
            format_version,
            collections: HashMap::new(),
            collection_names_by_id: HashMap::new(),
            primary_id_index: HashMap::new(),
            secondary_indexes: BTreeMap::new(),
            persistent_pages: HashMap::new(),
            next_page_id: 1,
            next_sequence: 1,
            next_collection_id: 1,
            next_document_id: 1,
            next_transaction_id: 1,
            next_index_id: 1,
            document_ids_exhausted: false,
            document_count: 0,
            write_failed: false,
            recovery_report: RecoveryReport::default(),
            mvcc: Arc::new(MvccStore::default()),
        }
    }

    fn reset_mvcc_to_current(&self) -> Result<()> {
        let version = if self.format_version >= MVCC_FORMAT_VERSION {
            CommitVersion::new(self.next_transaction_id.saturating_sub(1))
        } else {
            CommitVersion::new(0)
        };
        let collections: Vec<_> = self
            .collections
            .iter()
            .map(|(name, collection)| (name.clone(), collection.id))
            .collect();
        let documents: Vec<_> = self
            .collections
            .values()
            .flat_map(|collection| {
                collection
                    .documents
                    .iter()
                    .map(|(id, document)| (*id, collection.id, document.clone()))
            })
            .collect();
        self.mvcc.reset(version, &collections, &documents)
    }

    /// Returns the normalized path used to open the database.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the UUID bytes stored in the database header.
    pub const fn database_id(&self) -> [u8; 16] {
        self.database_id
    }

    /// Returns the on-disk file-format version used by this handle.
    pub const fn format_version(&self) -> u16 {
        self.format_version
    }

    /// Returns the current committed version ordinal.
    pub fn current_version(&self) -> Result<CommitVersion> {
        self.mvcc.current_version()
    }

    /// Begins a stable V3 read snapshot that can outlive a mutable writer borrow.
    pub fn begin_read(&self) -> Result<Snapshot> {
        if self.format_version < MVCC_FORMAT_VERSION {
            return Err(Error::UpgradeRequired(
                "write a V3 compacted copy before opening MVCC snapshots".into(),
            ));
        }
        self.mvcc.snapshot()
    }

    /// Starts a transaction in V2 or later. V1 files must first be copied to a transactional format.
    pub fn begin_transaction(&mut self) -> Result<Transaction<'_>> {
        self.check_usable()?;
        self.check_writable()?;
        if self.format_version < TRANSACTION_FORMAT_VERSION {
            return Err(Error::UpgradeRequired(
                "copy the V1 file to V2 or later before starting a transaction".into(),
            ));
        }
        if self.next_transaction_id <= 0 || self.next_transaction_id == i64::MAX {
            return Err(Error::Limit("transaction ID space is exhausted".into()));
        }
        let id = self.next_transaction_id;
        Ok(Transaction::new(self, id))
    }

    /// Reports any incomplete final record discarded during open.
    pub const fn recovery_report(&self) -> RecoveryReport {
        self.recovery_report
    }

    /// Checks current in-memory records and index trees without writing or repairing the file.
    pub fn check_integrity(&self) -> Result<IntegrityReport> {
        self.check_usable()?;
        let mut actual_documents = 0_usize;
        for (name, collection) in &self.collections {
            validate_collection_name(name)
                .map_err(|_| Error::Integrity("in-memory collection name is invalid".into()))?;
            if self.collection_names_by_id.get(&collection.id) != Some(name) {
                return Err(Error::Integrity(
                    "collection ID and name maps disagree".into(),
                ));
            }
            for (id, document) in &collection.documents {
                if *id <= 0 || self.primary_id_index.get(id) != Some(&collection.id) {
                    return Err(Error::Integrity(
                        "primary document index does not match a live document".into(),
                    ));
                }
                for value in document.fields().values() {
                    codec::validate_value(value).map_err(|error| {
                        Error::Integrity(format!("invalid in-memory document value: {error}"))
                    })?;
                }
                actual_documents = actual_documents
                    .checked_add(1)
                    .ok_or_else(|| Error::Integrity("document count overflow".into()))?;
            }
        }
        if actual_documents != self.document_count
            || self.primary_id_index.len() != self.document_count
        {
            return Err(Error::Integrity(
                "document count and primary index disagree".into(),
            ));
        }

        let committed_version = self.next_transaction_id.saturating_sub(1);
        for index in self.secondary_indexes.values() {
            let collection_name = self
                .collection_names_by_id
                .get(&index.collection_id)
                .ok_or_else(|| Error::Integrity("index names an unknown collection".into()))?;
            let collection = self
                .collections
                .get(collection_name)
                .ok_or_else(|| Error::Integrity("index collection is missing".into()))?;
            let mut expected = BTreeMap::new();
            for (document_id, document) in &collection.documents {
                if let Some(key) = index.key_for(document)? {
                    expected
                        .entry(key)
                        .or_insert_with(BTreeSet::new)
                        .insert(DocumentId(*document_id));
                }
            }
            if expected != index.entries {
                return Err(Error::Integrity(
                    "secondary index entries do not match collection documents".into(),
                ));
            }
            if self.format_version >= PERSISTENT_INDEX_FORMAT_VERSION {
                if index.root_page_id == 0 || index.root_generation > committed_version as u64 {
                    return Err(Error::Integrity("persistent index root is invalid".into()));
                }
                let actual: Vec<_> = btree::collect_entries(
                    index.root_page_id,
                    index.id.value(),
                    index.root_generation,
                    &self.persistent_pages,
                )?;
                let expected: Vec<_> = index
                    .entries
                    .iter()
                    .flat_map(|(value, ids)| {
                        ids.iter()
                            .map(|document_id| TreeKey::new(value.clone(), *document_id))
                    })
                    .collect();
                if actual != expected {
                    return Err(Error::Integrity(
                        "persistent B+Tree does not match its index and documents".into(),
                    ));
                }
            }
        }
        if self.format_version >= PERSISTENT_INDEX_FORMAT_VERSION {
            for (page_id, bytes) in &self.persistent_pages {
                let (decoded_id, _, generation) = btree::decode_stored_page(bytes)?;
                if decoded_id != *page_id || generation > committed_version as u64 {
                    return Err(Error::Integrity(
                        "stored page identity or generation is invalid".into(),
                    ));
                }
            }
        }
        Ok(IntegrityReport {
            format_version: self.format_version,
            database_id: self.database_id,
            file_bytes: self.file.metadata()?.len(),
            collection_count: self.collections.len(),
            document_count: self.document_count,
            index_count: self.secondary_indexes.len(),
            persistent_page_count: self.persistent_pages.len(),
            committed_version,
            discarded_tail_bytes: self.recovery_report.discarded_bytes(),
        })
    }

    /// Returns index metadata in stable ID order for inspection tools.
    pub fn indexes(&self) -> Result<Vec<IndexInfo>> {
        self.check_usable()?;
        self.secondary_indexes
            .values()
            .map(|index| {
                let collection = self
                    .collection_names_by_id
                    .get(&index.collection_id)
                    .cloned()
                    .ok_or_else(|| Error::Integrity("index names an unknown collection".into()))?;
                Ok(IndexInfo {
                    id: index.id,
                    collection,
                    path: index.path.clone(),
                    unique: index.unique,
                    root_page_id: (index.root_page_id != 0).then_some(index.root_page_id),
                    root_generation: (index.root_page_id != 0).then_some(index.root_generation),
                })
            })
            .collect()
    }

    /// Parses and executes the supported, document-aware SeaWorld SQL subset.
    /// SQL is translated to the same native query/planner and transaction APIs.
    pub fn execute_sql(
        &mut self,
        statement: &str,
        parameters: &[Value],
    ) -> Result<crate::SqlResult> {
        crate::sql::execute(self, statement, parameters)
    }

    /// Writes a consistent V4 compacted backup and never replaces an existing destination.
    pub fn backup(&self, target: impl AsRef<Path>) -> Result<()> {
        self.check_integrity()?;
        self.write_v4_compacted_copy(target)
    }

    /// Validates a backup through a fixed read-only view without repairing it.
    pub fn verify_backup(path: impl AsRef<Path>) -> Result<IntegrityReport> {
        let backup = Self::open_reader(path)?;
        let report = backup.check_integrity()?;
        backup.close()?;
        Ok(report)
    }

    /// Validates a backup read-only and restores it to a new V4 file without overwriting.
    pub fn restore_backup(source: impl AsRef<Path>, target: impl AsRef<Path>) -> Result<()> {
        let source = Self::open_reader(source)?;
        source.check_integrity()?;
        source.write_v4_compacted_copy(target)
    }

    /// Persists a new collection and returns its database-local identifier.
    pub fn create_collection(&mut self, name: &str) -> Result<CollectionId> {
        self.check_writable()?;
        if self.format_version >= TRANSACTION_FORMAT_VERSION {
            let mut transaction = self.begin_transaction()?;
            let id = transaction.create_collection(name)?;
            transaction.commit()?;
            return Ok(id);
        }
        self.check_usable()?;
        validate_collection_name(name)?;
        if self.collections.contains_key(name) {
            return Err(Error::Duplicate(format!(
                "collection {name:?} already exists"
            )));
        }
        if self.collections.len() >= MAX_COLLECTIONS {
            return Err(Error::Limit("collection count reached 10,000".into()));
        }
        if self.next_collection_id <= 0 {
            return Err(Error::Limit(
                "collection identifier space is exhausted".into(),
            ));
        }

        let collection_id = self.next_collection_id;
        let payload = encode_collection_create(collection_id, name)?;
        self.append_record(RecordType::CollectionCreate, &payload)?;
        self.collections.insert(
            name.to_owned(),
            CollectionState {
                id: collection_id,
                documents: BTreeMap::new(),
            },
        );
        self.collection_names_by_id
            .insert(collection_id, name.to_owned());
        self.next_collection_id = collection_id.checked_add(1).unwrap_or(0);
        Ok(CollectionId(collection_id))
    }

    /// Inserts a document and assigns the next positive database-local ID.
    pub fn insert(&mut self, collection_name: &str, document: Document) -> Result<DocumentId> {
        self.check_writable()?;
        if self.format_version >= TRANSACTION_FORMAT_VERSION {
            let mut transaction = self.begin_transaction()?;
            let id = transaction.insert(collection_name, document)?;
            transaction.commit()?;
            return Ok(id);
        }
        self.check_usable()?;
        if self.document_ids_exhausted || self.next_document_id <= 0 {
            return Err(Error::Limit(
                "document identifier space is exhausted".into(),
            ));
        }
        let id = DocumentId(self.next_document_id);
        self.insert_with_id(collection_name, id, document)?;
        Ok(id)
    }

    /// Inserts a document under a caller-supplied positive identifier.
    pub fn insert_with_id(
        &mut self,
        collection_name: &str,
        id: DocumentId,
        document: Document,
    ) -> Result<()> {
        self.check_writable()?;
        if self.format_version >= TRANSACTION_FORMAT_VERSION {
            let mut transaction = self.begin_transaction()?;
            transaction.insert_with_id(collection_name, id, document)?;
            transaction.commit()?;
            return Ok(());
        }
        self.check_usable()?;
        let collection_id = self.collection_id(collection_name)?;
        if self.primary_id_index.contains_key(&id.value()) {
            return Err(Error::Duplicate(format!("document ID {}", id.value())));
        }
        if self.document_count >= MAX_DOCUMENTS {
            return Err(Error::Limit("active document count reached 100,000".into()));
        }
        let payload = encode_document_mutation(collection_id, id, &document)?;
        self.append_record(RecordType::DocumentInsert, &payload)?;
        self.collections
            .get_mut(collection_name)
            .expect("collection checked above")
            .documents
            .insert(id.value(), document);
        self.primary_id_index.insert(id.value(), collection_id);
        self.document_count += 1;
        self.advance_next_document_id(id.value());
        Ok(())
    }

    /// Reads a document by primary ID from the named collection.
    pub fn get(&self, collection_name: &str, id: DocumentId) -> Result<Option<&Document>> {
        self.check_usable()?;
        let collection_id = self.collection_id(collection_name)?;
        if self.primary_id_index.get(&id.value()) != Some(&collection_id) {
            return Ok(None);
        }
        Ok(self
            .collections
            .get(collection_name)
            .and_then(|state| state.documents.get(&id.value())))
    }

    /// Replaces an existing document as one logged operation.
    pub fn update(
        &mut self,
        collection_name: &str,
        id: DocumentId,
        document: Document,
    ) -> Result<bool> {
        self.check_writable()?;
        if self.format_version >= TRANSACTION_FORMAT_VERSION {
            let mut transaction = self.begin_transaction()?;
            let updated = transaction.update(collection_name, id, document)?;
            if updated {
                transaction.commit()?;
            } else {
                transaction.rollback();
            }
            return Ok(updated);
        }
        self.check_usable()?;
        let collection_id = self.collection_id(collection_name)?;
        if self.primary_id_index.get(&id.value()) != Some(&collection_id) {
            return Ok(false);
        }
        let payload = encode_document_mutation(collection_id, id, &document)?;
        self.append_record(RecordType::DocumentUpdate, &payload)?;
        self.collections
            .get_mut(collection_name)
            .expect("collection checked above")
            .documents
            .insert(id.value(), document);
        Ok(true)
    }

    /// Deletes an existing document as one logged operation.
    pub fn delete(&mut self, collection_name: &str, id: DocumentId) -> Result<bool> {
        self.check_writable()?;
        if self.format_version >= TRANSACTION_FORMAT_VERSION {
            let mut transaction = self.begin_transaction()?;
            let deleted = transaction.delete(collection_name, id)?;
            if deleted {
                transaction.commit()?;
            } else {
                transaction.rollback();
            }
            return Ok(deleted);
        }
        self.check_usable()?;
        let collection_id = self.collection_id(collection_name)?;
        if self.primary_id_index.get(&id.value()) != Some(&collection_id) {
            return Ok(false);
        }
        let mut payload = Vec::with_capacity(16);
        payload.extend_from_slice(&collection_id.to_be_bytes());
        payload.extend_from_slice(&id.value().to_be_bytes());
        self.append_record(RecordType::DocumentDelete, &payload)?;
        self.collections
            .get_mut(collection_name)
            .expect("collection checked above")
            .documents
            .remove(&id.value());
        self.primary_id_index.remove(&id.value());
        self.document_count -= 1;
        Ok(true)
    }

    /// Returns active IDs in ascending order.
    pub fn document_ids(&self, collection_name: &str) -> Result<Vec<DocumentId>> {
        self.check_usable()?;
        let collection = self.collection(collection_name)?;
        Ok(collection
            .documents
            .keys()
            .copied()
            .map(DocumentId)
            .collect())
    }

    /// Returns collection names in ascending order.
    pub fn collection_names(&self) -> Result<Vec<String>> {
        self.check_usable()?;
        let mut names: Vec<_> = self.collections.keys().cloned().collect();
        names.sort();
        Ok(names)
    }

    /// Creates a persistent non-unique single-field index definition in V2 or later.
    pub fn create_index(&mut self, collection_name: &str, path: &str) -> Result<IndexId> {
        let path = FieldPath::parse(path)?;
        self.create_index_path(collection_name, path, false)
    }

    /// Creates a persistent unique single-field index definition in V2 or later.
    pub fn create_unique_index(&mut self, collection_name: &str, path: &str) -> Result<IndexId> {
        let path = FieldPath::parse(path)?;
        self.create_index_path(collection_name, path, true)
    }

    /// Creates an index from an already validated path.
    pub fn create_index_path(
        &mut self,
        collection_name: &str,
        path: FieldPath,
        unique: bool,
    ) -> Result<IndexId> {
        self.check_writable()?;
        self.check_usable()?;
        self.require_v2("indexes")?;
        let collection_id = self.collection_id(collection_name)?;
        if self.secondary_indexes.len() >= MAX_INDEXES {
            return Err(Error::Limit(format!(
                "database index count reached {MAX_INDEXES}"
            )));
        }
        if self
            .secondary_indexes
            .values()
            .any(|index| index.collection_id == collection_id && index.path == path)
        {
            return Err(Error::Duplicate(format!(
                "an index already exists on {collection_name}.{path}"
            )));
        }
        if self.next_index_id <= 0 || self.next_index_id == i64::MAX {
            return Err(Error::Limit("index identifier space is exhausted".into()));
        }
        let id = IndexId::from_positive(self.next_index_id);
        let mut index = SecondaryIndex::new(id, collection_id, path, unique);
        for (document_id, document) in &self
            .collections
            .get(collection_name)
            .expect("collection was checked")
            .documents
        {
            index.insert(DocumentId(*document_id), document)?;
        }

        if self.format_version >= PERSISTENT_INDEX_FORMAT_VERSION {
            let generation = self.next_transaction_id.saturating_sub(1) as u64;
            let keys = index.entries.iter().flat_map(|(value, ids)| {
                ids.iter()
                    .map(|document_id| TreeKey::new(value.clone(), *document_id))
            });
            let update = btree::build_tree(
                id.value(),
                generation,
                self.next_page_id,
                &self.persistent_pages,
                keys,
            )?;
            self.append_index_pages(&update.pages)?;
            index.root_page_id = update.root_page_id;
            index.root_generation = generation;
            self.next_page_id = update.next_page_id;
        }

        let payload = encode_index_definition(&index, self.format_version)?;
        self.append_record(RecordType::IndexCreate, &payload)?;
        self.secondary_indexes.insert(id.value(), index);
        self.next_index_id = id.value() + 1;
        Ok(id)
    }

    fn append_index_pages(&mut self, pages: &BTreeMap<u64, Vec<u8>>) -> Result<()> {
        for (page_id, bytes) in pages {
            if *page_id != self.next_page_id {
                return Err(Error::Integrity(
                    "new B+Tree page IDs must be contiguous".into(),
                ));
            }
            let (decoded_id, _, _) = btree::decode_stored_page(bytes)?;
            if decoded_id != *page_id {
                return Err(Error::Integrity(
                    "B+Tree page ID mismatch before write".into(),
                ));
            }
            self.append_record(RecordType::IndexPage, bytes)?;
            self.persistent_pages.insert(*page_id, bytes.clone());
            self.next_page_id = page_id
                .checked_add(1)
                .ok_or_else(|| Error::Limit("B+Tree page ID space exhausted".into()))?;
        }
        Ok(())
    }

    fn publish_index_batch(&mut self, transaction_id: i64, batch: PendingIndexBatch) -> Result<()> {
        if self.format_version < PERSISTENT_INDEX_FORMAT_VERSION {
            if !batch.pages.is_empty() || !batch.roots.is_empty() {
                return Err(Error::Integrity(
                    "legacy database contains persistent index records".into(),
                ));
            }
            return Ok(());
        }
        let mut overlay = BTreeMap::new();
        let mut expected_page_id = self.next_page_id;
        for (page_id, bytes) in &batch.pages {
            if *page_id != expected_page_id {
                return Err(Error::Integrity(
                    "persistent B+Tree page IDs are not sequential".into(),
                ));
            }
            let (decoded_id, owner, generation) = btree::decode_stored_page(bytes)?;
            if decoded_id != *page_id || generation != transaction_id as u64 {
                return Err(Error::Integrity(
                    "transaction B+Tree page has invalid metadata".into(),
                ));
            }
            if !self.secondary_indexes.contains_key(&owner) {
                return Err(Error::Integrity(
                    "transaction B+Tree page names an unknown index".into(),
                ));
            }
            overlay.insert(*page_id, bytes.clone());
            expected_page_id = expected_page_id
                .checked_add(1)
                .ok_or_else(|| Error::Integrity("B+Tree page ID overflow".into()))?;
        }
        if batch.next_page_id != expected_page_id {
            return Err(Error::Integrity(
                "transaction B+Tree page watermark is invalid".into(),
            ));
        }

        let mut updated = BTreeSet::new();
        for root in &batch.roots {
            if root.generation != transaction_id as u64
                || root.root_page_id == 0
                || !updated.insert(root.index_id)
            {
                return Err(Error::Integrity(
                    "invalid or duplicate B+Tree root publication".into(),
                ));
            }
            let index = self.secondary_indexes.get(&root.index_id).ok_or_else(|| {
                Error::Integrity("root publication names an unknown index".into())
            })?;
            let tree_entries = btree::collect_entries_with_overlay(
                root.root_page_id,
                root.index_id,
                root.generation,
                &self.persistent_pages,
                &overlay,
            )?;
            let expected_entries: Vec<_> = index
                .entries
                .iter()
                .flat_map(|(value, ids)| {
                    ids.iter()
                        .map(|document_id| TreeKey::new(value.clone(), *document_id))
                })
                .collect();
            if tree_entries != expected_entries {
                return Err(Error::Integrity(
                    "published B+Tree does not match committed index entries".into(),
                ));
            }
        }
        if !overlay.is_empty() && updated.is_empty() {
            return Err(Error::Integrity(
                "transaction contains unreachable B+Tree pages".into(),
            ));
        }
        for root in batch.roots {
            let index = self
                .secondary_indexes
                .get_mut(&root.index_id)
                .expect("root index was validated");
            index.root_page_id = root.root_page_id;
            index.root_generation = root.generation;
        }
        self.persistent_pages.extend(overlay);
        self.next_page_id = expected_page_id;
        Ok(())
    }

    /// Returns the primary, secondary, or collection-scan plan for a query.
    pub fn explain(&self, query: &Query) -> Result<QueryPlan> {
        self.validate_query(query)?;
        let collection_id = self.collection_id(query.collection())?;
        if let Filter::IdEq(id) = query.filter() {
            return Ok(QueryPlan::PrimaryKeyLookup {
                collection: query.collection().to_owned(),
                id: *id,
            });
        }
        for (path, value) in equality_terms(query.filter()) {
            let Ok(key) = IndexValue::from_value(value) else {
                continue;
            };
            if let Some(index) = self
                .secondary_indexes
                .values()
                .find(|index| index.collection_id == collection_id && index.path == *path)
            {
                let _ = key;
                return Ok(QueryPlan::SecondaryIndexLookup {
                    collection: query.collection().to_owned(),
                    index_id: index.id.value(),
                    path: index.path.clone(),
                    unique: index.unique,
                });
            }
        }
        for (path, operator, value) in range_terms(query.filter()) {
            if IndexValue::from_value(value).is_err() {
                continue;
            }
            if let Some(index) = self
                .secondary_indexes
                .values()
                .find(|index| index.collection_id == collection_id && index.path == *path)
            {
                return Ok(QueryPlan::SecondaryIndexRangeLookup {
                    collection: query.collection().to_owned(),
                    index_id: index.id.value(),
                    path: index.path.clone(),
                    operator,
                    value: value.clone(),
                    unique: index.unique,
                });
            }
        }
        Ok(QueryPlan::FullScan {
            collection: query.collection().to_owned(),
        })
    }

    /// Executes a native query and returns borrowed documents in deterministic order.
    pub fn execute(&self, query: &Query) -> Result<Vec<QueryRow<'_>>> {
        let plan = self.explain(query)?;
        let collection_id = self.collection_id(query.collection())?;
        let mut candidates: Vec<DocumentId> = match &plan {
            QueryPlan::PrimaryKeyLookup { id, .. } => {
                if self.primary_id_index.get(&id.value()) == Some(&collection_id) {
                    vec![*id]
                } else {
                    Vec::new()
                }
            }
            QueryPlan::SecondaryIndexLookup { index_id, path, .. } => {
                let value = equality_terms(query.filter())
                    .into_iter()
                    .find_map(|(candidate_path, value)| (candidate_path == path).then_some(value))
                    .ok_or_else(|| {
                        Error::Integrity("planned index predicate disappeared".into())
                    })?;
                let key = IndexValue::from_value(value)?;
                self.secondary_indexes
                    .get(index_id)
                    .and_then(|index| index.entries.get(&key))
                    .map(|ids| ids.iter().copied().collect())
                    .unwrap_or_default()
            }
            QueryPlan::SecondaryIndexRangeLookup {
                index_id,
                operator,
                value,
                ..
            } => {
                let key = IndexValue::from_value(value)?;
                self.secondary_indexes
                    .get(index_id)
                    .map(|index| index_range_candidates(index, &key, *operator))
                    .unwrap_or_default()
            }
            QueryPlan::FullScan { .. } => self
                .collections
                .get(query.collection())
                .expect("collection was checked")
                .documents
                .keys()
                .copied()
                .map(DocumentId)
                .collect(),
        };

        candidates.sort_unstable();
        let collection = self
            .collections
            .get(query.collection())
            .expect("collection was checked");
        let mut rows = Vec::new();
        for id in candidates {
            if let Some(document) = collection.documents.get(&id.value())
                && query::matches(query.filter(), id, document)
            {
                rows.push(QueryRow { id, document });
            }
        }
        if let Some((path, direction)) = &query.sort {
            let mut keyed = rows
                .into_iter()
                .map(|row| {
                    let key = path
                        .get(row.document)
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
                if order == Ordering::Equal {
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

    /// Executes a native query and projects its results after sorting and pagination.
    pub fn execute_projected(
        &self,
        query: &Query,
        projection: &crate::Projection,
    ) -> Result<Vec<crate::ProjectedRow>> {
        Ok(self
            .execute(query)?
            .into_iter()
            .map(|row| projection.apply(row.id, row.document))
            .collect())
    }

    /// Applies operations to the first matching document in one transaction.
    pub fn update_one(
        &mut self,
        query: &Query,
        operations: &[crate::DocumentOperation],
    ) -> Result<usize> {
        self.validate_query(query)?;
        self.update_many(&query.clone().limit(query.limit.min(1)), operations)
    }

    /// Updates the first selected document or inserts an explicit base document.
    pub fn upsert_one(
        &mut self,
        query: &Query,
        insert_document: Document,
        operations: &[crate::DocumentOperation],
    ) -> Result<(DocumentId, bool)> {
        self.check_writable()?;
        self.validate_query(query)?;
        if query.limit == 0 {
            return Err(Error::Query("upsert requires a nonzero query limit".into()));
        }
        let first = self.execute(&query.clone().limit(1))?.first().map(|row| row.id);
        let mut transaction = self.begin_transaction()?;
        let result = if let Some(id) = first {
            transaction.update_fields(query.collection(), id, operations)?;
            (id, false)
        } else {
            let document = crate::apply_document_operations(&insert_document, operations)?;
            (transaction.insert(query.collection(), document)?, true)
        };
        transaction.commit()?;
        Ok(result)
    }

    /// Applies operations to the selected committed view in one transaction.
    pub fn update_many(
        &mut self,
        query: &Query,
        operations: &[crate::DocumentOperation],
    ) -> Result<usize> {
        self.check_writable()?;
        if self.format_version < TRANSACTION_FORMAT_VERSION {
            return Err(Error::UpgradeRequired(
                "transactional format required".into(),
            ));
        }
        let ids: Vec<_> = self.execute(query)?.into_iter().map(|row| row.id).collect();
        if ids.is_empty() {
            return Ok(0);
        }
        let mut transaction = self.begin_transaction()?;
        for id in &ids {
            transaction.update_fields(query.collection(), *id, operations)?;
        }
        transaction.commit()?;
        Ok(ids.len())
    }

    /// Deletes the first matching document in one transaction.
    pub fn delete_one(&mut self, query: &Query) -> Result<usize> {
        self.validate_query(query)?;
        self.delete_many(&query.clone().limit(query.limit.min(1)))
    }

    /// Deletes the selected committed view in one transaction.
    pub fn delete_many(&mut self, query: &Query) -> Result<usize> {
        self.check_writable()?;
        if self.format_version < TRANSACTION_FORMAT_VERSION {
            return Err(Error::UpgradeRequired(
                "transactional format required".into(),
            ));
        }
        let ids: Vec<_> = self.execute(query)?.into_iter().map(|row| row.id).collect();
        if ids.is_empty() {
            return Ok(0);
        }
        let mut transaction = self.begin_transaction()?;
        for id in &ids {
            transaction.delete(query.collection(), *id)?;
        }
        transaction.commit()?;
        Ok(ids.len())
    }

    fn validate_query(&self, query: &Query) -> Result<()> {
        self.check_usable()?;
        query::validate_filter(query.filter())?;
        validate_collection_name(query.collection())?;
        if query.limit > MAX_QUERY_RESULTS || query.offset > MAX_QUERY_RESULTS {
            return Err(Error::Limit(format!(
                "query limit and offset cannot exceed {MAX_QUERY_RESULTS}"
            )));
        }
        if let Some((path, _)) = &query.sort
            && path.segments().len() > crate::MAX_FIELD_PATH_DEPTH
        {
            return Err(Error::Limit("sort path is too deep".into()));
        }
        Ok(())
    }

    fn require_v2(&self, feature: &str) -> Result<()> {
        if self.format_version < TRANSACTION_FORMAT_VERSION {
            return Err(Error::UpgradeRequired(format!(
                "write a V2 or later compacted copy before using {feature}"
            )));
        }
        Ok(())
    }

    fn commit_transaction(
        &mut self,
        id: i64,
        operations: &[StagedOperation],
        overlay: &HashMap<i64, OverlayDocument>,
    ) -> Result<PendingIndexBatch> {
        self.check_usable()?;
        self.require_v2("transactions")?;
        if id <= 0 || id != self.next_transaction_id || id == i64::MAX {
            return Err(Error::TransactionState(
                "transaction identifier is not the next available identifier".into(),
            ));
        }
        if operations.len() > MAX_TRANSACTION_OPERATIONS {
            return Err(Error::Limit("transaction operation limit exceeded".into()));
        }
        if self.format_version >= MVCC_FORMAT_VERSION {
            let changes: Vec<_> = overlay
                .iter()
                .map(|(document_id, entry)| {
                    (*document_id, entry.collection_id, entry.document.clone())
                })
                .collect();
            self.mvcc
                .ensure_commit_capacity(CommitVersion::new(id), &changes)?;
        }

        let index_batch = self.prepare_index_batch(id, overlay)?;

        let mut blocks = Vec::with_capacity(
            operations.len() + index_batch.pages.len() + index_batch.roots.len() + 2,
        );
        let begin_payload = id.to_be_bytes();
        let mut sequence = self.next_sequence;
        blocks.push(encode_record(
            RecordType::TransactionBegin,
            sequence,
            &begin_payload,
        )?);
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| Error::Limit("record sequence space is exhausted".into()))?;

        let mut transaction_bytes = 0_usize;
        for operation in operations {
            let mut payload = Vec::with_capacity(8 + 24);
            payload.extend_from_slice(&id.to_be_bytes());
            payload.extend_from_slice(&operation.payload()?);
            transaction_bytes = transaction_bytes
                .checked_add(payload.len())
                .ok_or_else(|| Error::Limit("transaction payload length overflow".into()))?;
            if transaction_bytes > MAX_TRANSACTION_BYTES {
                return Err(Error::Limit("transaction byte limit exceeded".into()));
            }
            blocks.push(encode_record(operation.record_type(), sequence, &payload)?);
            sequence = sequence
                .checked_add(1)
                .ok_or_else(|| Error::Limit("record sequence space is exhausted".into()))?;
        }
        let mut transaction_bytes = blocks
            .iter()
            .skip(1)
            .map(|block| block.len() - MIN_RECORD_SIZE)
            .try_fold(0_usize, |size, payload| {
                size.checked_add(payload)
                    .ok_or_else(|| Error::Limit("transaction byte length overflow".into()))
            })?;
        if transaction_bytes > MAX_TRANSACTION_BYTES {
            return Err(Error::Limit("transaction byte limit exceeded".into()));
        }
        for (page_id, page) in &index_batch.pages {
            let mut payload = Vec::with_capacity(8 + page.len());
            payload.extend_from_slice(&id.to_be_bytes());
            payload.extend_from_slice(page);
            transaction_bytes = transaction_bytes
                .checked_add(payload.len())
                .ok_or_else(|| Error::Limit("transaction byte length overflow".into()))?;
            if transaction_bytes > MAX_TRANSACTION_BYTES {
                return Err(Error::Limit("transaction byte limit exceeded".into()));
            }
            let _ = page_id;
            blocks.push(encode_record(RecordType::IndexPage, sequence, &payload)?);
            sequence = sequence
                .checked_add(1)
                .ok_or_else(|| Error::Limit("record sequence space is exhausted".into()))?;
        }
        for root in &index_batch.roots {
            let mut payload = Vec::with_capacity(32);
            payload.extend_from_slice(&id.to_be_bytes());
            payload.extend_from_slice(&root.index_id.to_be_bytes());
            payload.extend_from_slice(&(root.root_page_id as i64).to_be_bytes());
            payload.extend_from_slice(&(root.generation as i64).to_be_bytes());
            transaction_bytes = transaction_bytes
                .checked_add(payload.len())
                .ok_or_else(|| Error::Limit("transaction byte length overflow".into()))?;
            if transaction_bytes > MAX_TRANSACTION_BYTES {
                return Err(Error::Limit("transaction byte limit exceeded".into()));
            }
            blocks.push(encode_record(RecordType::IndexRoot, sequence, &payload)?);
            sequence = sequence
                .checked_add(1)
                .ok_or_else(|| Error::Limit("record sequence space is exhausted".into()))?;
        }
        blocks.push(encode_record(
            RecordType::TransactionCommit,
            sequence,
            &id.to_be_bytes(),
        )?);
        let next_sequence = sequence
            .checked_add(1)
            .ok_or_else(|| Error::Limit("record sequence space is exhausted".into()))?;
        let batch_size = blocks.iter().try_fold(0_u64, |size, block| {
            size.checked_add(block.len() as u64)
                .ok_or_else(|| Error::Limit("transaction file length overflow".into()))
        })?;
        let current_size = self.file.metadata()?.len();
        if current_size
            .checked_add(batch_size)
            .is_none_or(|size| size > MAX_DATABASE_BYTES)
        {
            return Err(Error::Limit("database file size limit reached".into()));
        }

        self.file.lock()?;
        if let Err(error) = self.file.seek(SeekFrom::End(0)) {
            let _ = self.file.unlock();
            return Err(Error::Io(error));
        }
        let write_result = (|| -> std::io::Result<()> {
            crash_before_write();
            crash_transaction_at("transaction_before_begin");
            if crash_during_write() {
                let begin = &blocks[0];
                self.file.write_all(&begin[..(begin.len() / 2).max(1)])?;
                crash_pause("during_write");
            }
            self.file.write_all(&blocks[0])?;
            crash_transaction_at("transaction_after_begin");
            for (index, block) in blocks[1..blocks.len() - 1].iter().enumerate() {
                let first_page_record = index == operations.len();
                let page_record =
                    index >= operations.len() && index < operations.len() + index_batch.pages.len();
                let first_root_record = index == operations.len() + index_batch.pages.len();
                if first_page_record {
                    crash_transaction_at("transaction_before_index_page_write");
                }
                if first_root_record {
                    crash_transaction_at("transaction_before_root_publication");
                }
                if index == 0 && crash_transaction_during("transaction_during_first_operation") {
                    self.file.write_all(&block[..(block.len() / 2).max(1)])?;
                    crash_pause("transaction_during_first_operation");
                }
                if first_page_record
                    && crash_transaction_during("transaction_during_index_page_write")
                {
                    self.file.write_all(&block[..(block.len() / 2).max(1)])?;
                    crash_pause("transaction_during_index_page_write");
                }
                if first_root_record
                    && crash_transaction_during("transaction_during_root_publication")
                {
                    self.file.write_all(&block[..(block.len() / 2).max(1)])?;
                    crash_pause("transaction_during_root_publication");
                }
                self.file.write_all(block)?;
                if page_record {
                    crash_transaction_at("transaction_after_index_page_write");
                }
                if index + 1 == operations.len() + index_batch.pages.len() {
                    crash_transaction_at("transaction_after_index_pages_before_root");
                }
                if index + 1 == operations.len() + index_batch.pages.len() + index_batch.roots.len()
                {
                    crash_transaction_at("transaction_after_root_before_commit");
                }
                if index == 0 && blocks.len() > 3 {
                    crash_transaction_at("transaction_between_operations");
                }
                if index + 1 == blocks.len() - 2 {
                    crash_transaction_at("transaction_after_last_operation");
                }
            }
            if blocks.len() == 2 {
                crash_transaction_at("transaction_after_last_operation");
            }
            let commit = blocks.last().expect("begin and commit records exist");
            if crash_transaction_during("transaction_during_commit_record") {
                self.file.write_all(&commit[..(commit.len() / 2).max(1)])?;
                crash_pause("transaction_during_commit_record");
            }
            self.file.write_all(commit)?;
            crash_after_write();
            crash_transaction_at("transaction_after_commit_write_before_sync");
            self.file.sync_all()?;
            crash_after_sync();
            crash_transaction_at("transaction_after_sync_before_return");
            Ok(())
        })();
        let unlock_result = self.file.unlock();
        if let Err(error) = write_result {
            self.write_failed = true;
            return Err(Error::Io(error));
        }
        if let Err(error) = unlock_result {
            self.write_failed = true;
            return Err(Error::Io(error));
        }
        self.next_sequence = next_sequence;
        Ok(index_batch)
    }

    fn prepare_index_batch(
        &self,
        transaction_id: i64,
        overlay: &HashMap<i64, OverlayDocument>,
    ) -> Result<PendingIndexBatch> {
        let mut batch = PendingIndexBatch {
            next_page_id: self.next_page_id,
            ..PendingIndexBatch::default()
        };
        if self.format_version < PERSISTENT_INDEX_FORMAT_VERSION {
            return Ok(batch);
        }
        let deltas = self.index_updates_for_overlay(overlay)?;
        let mut by_index: BTreeMap<i64, Vec<(TreeKey, bool)>> = BTreeMap::new();
        for delta in deltas {
            if let Some(key) = delta.old_key {
                by_index
                    .entry(delta.index_id)
                    .or_default()
                    .push((TreeKey::new(key, delta.document_id), false));
            }
            if let Some(key) = delta.new_key {
                by_index
                    .entry(delta.index_id)
                    .or_default()
                    .push((TreeKey::new(key, delta.document_id), true));
            }
        }
        for (index_id, changes) in by_index {
            let index = self
                .secondary_indexes
                .get(&index_id)
                .ok_or_else(|| Error::Integrity("index delta names an unknown index".into()))?;
            if index.root_page_id == 0 {
                return Err(Error::Integrity("persistent index has no root page".into()));
            }
            let update = btree::apply_changes(
                index_id,
                index.root_page_id,
                transaction_id as u64,
                batch.next_page_id,
                &self.persistent_pages,
                changes,
            )?;
            if update.root_page_id != index.root_page_id {
                batch.roots.push(RootPublication {
                    index_id,
                    root_page_id: update.root_page_id,
                    generation: transaction_id as u64,
                });
            }
            batch.next_page_id = update.next_page_id;
            batch.pages.extend(update.pages);
        }
        Ok(batch)
    }

    fn apply_transaction_state(
        &mut self,
        transaction_id: i64,
        operations: &[StagedOperation],
        overlay: &HashMap<i64, OverlayDocument>,
        next_collection_id: i64,
        next_document_id: i64,
        document_ids_exhausted: bool,
    ) -> Result<()> {
        if transaction_id != self.next_transaction_id || transaction_id == i64::MAX {
            return Err(Error::Integrity(
                "transaction identifier is not the next available identifier".into(),
            ));
        }
        if next_document_id <= 0
            || next_document_id < self.next_document_id
            || (self.document_ids_exhausted && !document_ids_exhausted)
        {
            return Err(Error::Integrity(
                "transaction moved document-ID state backwards or made it invalid".into(),
            ));
        }

        let mut created_collections = Vec::new();
        let mut created_ids = BTreeSet::new();
        let mut expected_collection_id = self.next_collection_id;
        for operation in operations {
            if let StagedOperation::CollectionCreate { id, name } = operation {
                validate_collection_name(name).map_err(|_| {
                    Error::Integrity("invalid collection name in transaction".into())
                })?;
                if *id != expected_collection_id
                    || self.collections.contains_key(name)
                    || self.collection_names_by_id.contains_key(id)
                    || created_collections
                        .iter()
                        .any(|(_, existing): &(i64, String)| existing == name)
                    || !created_ids.insert(*id)
                {
                    return Err(Error::Integrity(
                        "committed transaction creates an invalid or duplicate collection".into(),
                    ));
                }
                expected_collection_id =
                    expected_collection_id.checked_add(1).ok_or_else(|| {
                        Error::Integrity("collection identifier space is exhausted".into())
                    })?;
                created_collections.push((*id, name.clone()));
            }
        }
        if expected_collection_id != next_collection_id
            || self.collections.len() + created_collections.len() > MAX_COLLECTIONS
        {
            return Err(Error::Integrity(
                "transaction produced an invalid collection state".into(),
            ));
        }

        let mut new_document_count = self.document_count;
        let mut document_ids: Vec<_> = overlay.keys().copied().collect();
        document_ids.sort_unstable();
        for document_id in &document_ids {
            if let Some(old_collection_id) = self.primary_id_index.get(document_id).copied() {
                let old_name = self
                    .collection_names_by_id
                    .get(&old_collection_id)
                    .ok_or_else(|| {
                        Error::Integrity("primary index names an unknown collection".into())
                    })?;
                if !self
                    .collections
                    .get(old_name)
                    .is_some_and(|collection| collection.documents.contains_key(document_id))
                {
                    return Err(Error::Integrity(
                        "primary index names a missing document".into(),
                    ));
                }
                new_document_count = new_document_count
                    .checked_sub(1)
                    .ok_or_else(|| Error::Integrity("active document count underflow".into()))?;
            }
            let entry = overlay
                .get(document_id)
                .ok_or_else(|| Error::Integrity("transaction overlay lost a document".into()))?;
            if entry.document.is_some() {
                if !self
                    .collection_names_by_id
                    .contains_key(&entry.collection_id)
                    && !created_ids.contains(&entry.collection_id)
                {
                    return Err(Error::Integrity(
                        "transaction names an unknown target collection".into(),
                    ));
                }
                new_document_count = new_document_count
                    .checked_add(1)
                    .ok_or_else(|| Error::Integrity("active document count overflow".into()))?;
            }
        }
        if new_document_count > MAX_DOCUMENTS {
            return Err(Error::Integrity(
                "active document limit exceeded in committed transaction".into(),
            ));
        }

        let index_updates = self.index_updates_for_overlay(overlay)?;

        for update in &index_updates {
            if let Some(key) = &update.old_key {
                self.secondary_indexes
                    .get_mut(&update.index_id)
                    .expect("prevalidated index exists")
                    .remove_key(update.document_id, key);
            }
        }
        for update in &index_updates {
            if let Some(key) = &update.new_key {
                self.secondary_indexes
                    .get_mut(&update.index_id)
                    .expect("prevalidated index exists")
                    .insert_key(update.document_id, key.clone());
            }
        }

        for (id, name) in created_collections {
            self.collections.insert(
                name.clone(),
                CollectionState {
                    id,
                    documents: BTreeMap::new(),
                },
            );
            self.collection_names_by_id.insert(id, name);
        }
        for document_id in document_ids {
            if let Some(old_collection_id) = self.primary_id_index.remove(&document_id) {
                let old_name = self
                    .collection_names_by_id
                    .get(&old_collection_id)
                    .expect("old collection was prevalidated");
                self.collections
                    .get_mut(old_name)
                    .expect("old collection was prevalidated")
                    .documents
                    .remove(&document_id);
            }
            let entry = overlay.get(&document_id).expect("overlay ID was collected");
            if let Some(document) = &entry.document {
                let name = self
                    .collection_names_by_id
                    .get(&entry.collection_id)
                    .expect("target collection was prevalidated");
                self.collections
                    .get_mut(name)
                    .expect("target collection was prevalidated")
                    .documents
                    .insert(document_id, document.clone());
                self.primary_id_index
                    .insert(document_id, entry.collection_id);
            }
        }
        self.document_count = new_document_count;
        self.next_collection_id = next_collection_id;
        self.next_document_id = next_document_id;
        self.document_ids_exhausted = document_ids_exhausted;
        self.next_transaction_id = transaction_id + 1;
        if self.format_version >= MVCC_FORMAT_VERSION {
            let new_collections: Vec<_> = operations
                .iter()
                .filter_map(|operation| match operation {
                    StagedOperation::CollectionCreate { id, name } => Some((name.clone(), *id)),
                    _ => None,
                })
                .collect();
            let changes: Vec<_> = overlay
                .iter()
                .map(|(document_id, entry)| {
                    (*document_id, entry.collection_id, entry.document.clone())
                })
                .collect();
            if let Err(error) = self.mvcc.apply_commit(
                CommitVersion::new(transaction_id),
                &new_collections,
                &changes,
            ) {
                self.write_failed = true;
                return Err(error);
            }
        }
        Ok(())
    }

    fn index_updates_for_overlay(
        &self,
        overlay: &HashMap<i64, OverlayDocument>,
    ) -> Result<Vec<IndexDelta>> {
        let mut document_ids: Vec<_> = overlay.keys().copied().collect();
        document_ids.sort_unstable();
        let mut updates = Vec::new();
        for index in self.secondary_indexes.values() {
            let mut final_unique_keys = BTreeMap::new();
            for document_id in &document_ids {
                let entry = overlay.get(document_id).expect("overlay ID was collected");
                let old_key =
                    if self.primary_id_index.get(document_id) == Some(&index.collection_id) {
                        let old_name = self
                            .collection_names_by_id
                            .get(&index.collection_id)
                            .ok_or_else(|| {
                                Error::Integrity("index names an unknown collection".into())
                            })?;
                        let old_document = self
                            .collections
                            .get(old_name)
                            .and_then(|collection| collection.documents.get(document_id))
                            .ok_or_else(|| {
                                Error::Integrity("index source document is missing".into())
                            })?;
                        index.key_for(old_document)?
                    } else {
                        None
                    };
                let new_key = if entry.collection_id == index.collection_id {
                    entry
                        .document
                        .as_ref()
                        .map(|document| index.key_for(document))
                        .transpose()?
                        .flatten()
                } else {
                    None
                };
                if index.unique
                    && let Some(key) = &new_key
                {
                    if index.entries.get(key).is_some_and(|ids| {
                        ids.iter()
                            .any(|existing| !overlay.contains_key(&existing.value()))
                    }) {
                        return Err(Error::ConstraintViolation(format!(
                            "unique index {} rejects duplicate value",
                            index.path
                        )));
                    }
                    if final_unique_keys
                        .insert(key.clone(), *document_id)
                        .is_some()
                    {
                        return Err(Error::ConstraintViolation(format!(
                            "unique index {} rejects duplicate values in transaction",
                            index.path
                        )));
                    }
                }
                if old_key != new_key {
                    updates.push(IndexDelta {
                        index_id: index.id.value(),
                        document_id: DocumentId(*document_id),
                        old_key,
                        new_key,
                    });
                }
            }
        }
        Ok(updates)
    }

    /// Writes a compacted copy in the source file version without replacing it.
    ///
    /// The new file preserves collection/document IDs and the database UUID, but
    /// rewrites only currently active documents. It is published without
    /// overwriting an existing destination. The destination filesystem must
    /// support hard links; otherwise this returns an I/O error and leaves this
    /// database unchanged.
    pub fn write_compacted_copy(&self, target: impl AsRef<Path>) -> Result<()> {
        self.check_usable()?;
        if self.format_version >= MVCC_FORMAT_VERSION {
            if self.mvcc.has_active_snapshots()? {
                return Err(Error::ActiveSnapshots);
            }
            return self.write_v3_compacted_copy(target);
        }
        if self.format_version == TRANSACTION_FORMAT_VERSION {
            return self.write_v2_compacted_copy(target);
        }
        let target = absolute_path(target.as_ref())?;
        if target == self.path {
            return Err(Error::InvalidInput(
                "compacted copy must use a different path".into(),
            ));
        }
        if target.exists() {
            return Err(Error::Duplicate(format!(
                "compacted-copy destination already exists: {}",
                target.display()
            )));
        }
        let temporary = temporary_sibling_path(&target)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;

        let write_result = (|| -> Result<()> {
            file.write_all(&format::encode_header(self.database_id))?;
            let mut file_size = HEADER_SIZE as u64;
            let mut sequence = 1_i64;
            let mut collections: Vec<_> = self
                .collection_names_by_id
                .iter()
                .map(|(id, name)| (*id, name.as_str()))
                .collect();
            collections.sort_unstable_by_key(|(id, _)| *id);
            if collections.len() > MAX_COLLECTIONS {
                return Err(Error::Integrity(
                    "in-memory collection count exceeds the file limit".into(),
                ));
            }

            let mut expected_collection_id = 1_i64;
            for (collection_id, name) in collections {
                if collection_id != expected_collection_id {
                    return Err(Error::Integrity(
                        "collection IDs are not contiguous during compaction".into(),
                    ));
                }
                expected_collection_id = expected_collection_id
                    .checked_add(1)
                    .ok_or_else(|| Error::Limit("collection identifier overflow".into()))?;
                let collection = self.collections.get(name).ok_or_else(|| {
                    Error::Integrity("collection ID map is inconsistent during compaction".into())
                })?;
                if collection.id != collection_id {
                    return Err(Error::Integrity(
                        "collection ID map is inconsistent during compaction".into(),
                    ));
                }
                write_snapshot_record(
                    &mut file,
                    &mut sequence,
                    &mut file_size,
                    RecordType::CollectionCreate,
                    &encode_collection_create(collection_id, name)?,
                )?;
                for (document_id, document) in &collection.documents {
                    write_snapshot_record(
                        &mut file,
                        &mut sequence,
                        &mut file_size,
                        RecordType::DocumentInsert,
                        &encode_document_mutation(
                            collection_id,
                            DocumentId(*document_id),
                            document,
                        )?,
                    )?;
                }
            }
            file.sync_all()?;
            Ok(())
        })();
        drop(file);
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }

        if let Err(error) = fs::hard_link(&temporary, &target) {
            let _ = fs::remove_file(&temporary);
            return Err(Error::Io(error));
        }
        if let Err(error) = sync_parent_directory(&target) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        let _ = fs::remove_file(&temporary);
        let _ = sync_parent_directory(&target);
        Ok(())
    }

    /// Writes a new V2 snapshot without replacing this file.
    ///
    /// This also upgrades a V1 source. It preserves the UUID, live IDs,
    /// secondary index definitions, and the document-ID high-water mark. The
    /// destination is published without overwriting an existing path.
    pub fn write_v2_compacted_copy(&self, target: impl AsRef<Path>) -> Result<()> {
        self.write_transactional_compacted_copy(target, TRANSACTION_FORMAT_VERSION)
    }

    /// Writes a V3 snapshot preserving the source commit-version watermark.
    /// Active snapshots must be released before compaction can proceed.
    pub fn write_v3_compacted_copy(&self, target: impl AsRef<Path>) -> Result<()> {
        if self.mvcc.has_active_snapshots()? {
            return Err(Error::ActiveSnapshots);
        }
        self.write_transactional_compacted_copy(target, MVCC_FORMAT_VERSION)
    }

    /// Writes a V4 snapshot with persistent copy-on-write index pages.
    /// Active snapshots must be released before compaction can proceed.
    pub fn write_v4_compacted_copy(&self, target: impl AsRef<Path>) -> Result<()> {
        if self.mvcc.has_active_snapshots()? {
            return Err(Error::ActiveSnapshots);
        }
        self.write_transactional_compacted_copy(target, PERSISTENT_INDEX_FORMAT_VERSION)
    }

    fn write_transactional_compacted_copy(
        &self,
        target: impl AsRef<Path>,
        target_version: u16,
    ) -> Result<()> {
        self.check_usable()?;
        let target = absolute_path(target.as_ref())?;
        if target == self.path {
            return Err(Error::InvalidInput(
                "compacted copy must use a different path".into(),
            ));
        }
        if target.exists() {
            return Err(Error::Duplicate(format!(
                "compacted-copy destination already exists: {}",
                target.display()
            )));
        }

        let temporary = temporary_sibling_path(&target)?;
        let write_result = (|| -> Result<()> {
            let mut copy =
                Self::create_with_identity(&temporary, self.database_id, target_version)?;
            crash_pause_if("compaction_after_create");
            let mut collections: Vec<_> = self
                .collection_names_by_id
                .iter()
                .map(|(id, name)| (*id, name.as_str()))
                .collect();
            collections.sort_unstable_by_key(|(id, _)| *id);
            if collections.len() > MAX_COLLECTIONS {
                return Err(Error::Integrity(
                    "in-memory collection count exceeds the file limit".into(),
                ));
            }

            for (expected_id, name) in collections {
                let actual_id = copy.create_collection(name)?;
                if actual_id.value() != expected_id {
                    return Err(Error::Integrity(
                        "collection IDs are not contiguous during V2 compaction".into(),
                    ));
                }
                let documents = &self
                    .collections
                    .get(name)
                    .ok_or_else(|| {
                        Error::Integrity(
                            "collection ID map is inconsistent during V2 compaction".into(),
                        )
                    })?
                    .documents;
                let mut iterator = documents.iter().peekable();
                while iterator.peek().is_some() {
                    let mut transaction = copy.begin_transaction()?;
                    let mut operation_count = 0_usize;
                    let mut operation_bytes = 0_usize;
                    while let Some((document_id, document)) = iterator.peek().copied() {
                        let encoded_length = codec::encode_document(document)?.len();
                        let encoded_length = encoded_length
                            .checked_add(28)
                            .ok_or_else(|| Error::Limit("transaction size overflow".into()))?;
                        if operation_count > 0
                            && (operation_count >= MAX_TRANSACTION_OPERATIONS
                                || operation_bytes
                                    .checked_add(encoded_length)
                                    .is_none_or(|size| size > MAX_TRANSACTION_BYTES))
                        {
                            break;
                        }
                        if encoded_length > MAX_TRANSACTION_BYTES {
                            return Err(Error::Limit(
                                "one document exceeds the V2 transaction byte limit".into(),
                            ));
                        }
                        transaction.insert_with_id(
                            name,
                            DocumentId(*document_id),
                            (*document).clone(),
                        )?;
                        iterator.next();
                        operation_count += 1;
                        operation_bytes += encoded_length;
                    }
                    transaction.commit()?;
                }
            }

            let mut indexes: Vec<_> = self.secondary_indexes.values().collect();
            indexes.sort_unstable_by_key(|index| index.id.value());
            for index in indexes {
                let name = self
                    .collection_names_by_id
                    .get(&index.collection_id)
                    .ok_or_else(|| Error::Integrity("index names an unknown collection".into()))?;
                let created = copy.create_index_path(name, index.path.clone(), index.unique)?;
                if created.value() != index.id.value() {
                    return Err(Error::Integrity(
                        "index IDs are not contiguous during V2 compaction".into(),
                    ));
                }
            }
            let mut watermark = self.next_document_id.to_be_bytes().to_vec();
            watermark.push(u8::from(self.document_ids_exhausted));
            copy.append_record(RecordType::DocumentIdWatermark, &watermark)?;
            if target_version >= MVCC_FORMAT_VERSION {
                let source_version = if self.format_version >= MVCC_FORMAT_VERSION {
                    self.mvcc.current_version()?.value()
                } else {
                    0
                };
                let committed_version = source_version.max(copy.next_transaction_id - 1);
                if committed_version > 0 {
                    copy.append_record(
                        RecordType::VersionWatermark,
                        &committed_version.to_be_bytes(),
                    )?;
                }
            }
            copy.close()?;
            let verified = Self::open_reader(&temporary)?;
            verified.check_integrity()?;
            verified.close()?;
            crash_pause_if("compaction_before_publish");
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary);
            let _ = fs::remove_file(sidecar_path(&temporary, ".writer.lock")?);
            return Err(error);
        }

        let _ = fs::remove_file(sidecar_path(&temporary, ".writer.lock")?);

        if let Err(error) = fs::hard_link(&temporary, &target) {
            let _ = fs::remove_file(&temporary);
            return Err(Error::Io(error));
        }
        if let Err(error) = sync_parent_directory(&target) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        let _ = fs::remove_file(&temporary);
        let _ = sync_parent_directory(&target);
        Ok(())
    }

    /// Synchronizes a writable handle and releases its exclusive writer lock.
    pub fn close(self) -> Result<()> {
        let sync_result = if self.writable {
            self.file.sync_all()
        } else {
            Ok(())
        };
        let unlock_result = self.writer_lock.as_ref().map_or(Ok(()), File::unlock);
        match (sync_result, unlock_result) {
            (Err(error), _) => Err(Error::Io(error)),
            (Ok(()), Err(error)) => Err(Error::Io(error)),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    fn replay_records(&mut self, file_size: u64) -> Result<()> {
        if self.format_version == FORMAT_VERSION {
            return self.replay_v1_records(file_size);
        }
        self.replay_v2_records(file_size)
    }

    fn replay_v1_records(&mut self, file_size: u64) -> Result<()> {
        let mut offset = HEADER_SIZE as u64;
        while offset < file_size {
            let remaining = file_size - offset;
            if remaining < RECORD_PREFIX_SIZE as u64 {
                return self.recover_incomplete_tail(offset, file_size);
            }

            self.file.seek(SeekFrom::Start(offset))?;
            let mut prefix = [0_u8; RECORD_PREFIX_SIZE];
            self.file.read_exact(&mut prefix).map_err(|error| {
                if error.kind() == std::io::ErrorKind::UnexpectedEof {
                    Error::Integrity("database changed or truncated while opening".into())
                } else {
                    Error::Io(error)
                }
            })?;
            if &prefix[..4] != RECORD_MAGIC {
                return Err(Error::Integrity(format!(
                    "invalid record magic at offset {offset}"
                )));
            }
            let type_code = prefix[4];
            if prefix[5] != 0 || prefix[6] != 0 || prefix[7] != 0 {
                return Err(Error::Format(format!(
                    "unsupported record flags at offset {offset}"
                )));
            }
            let block_length = i32::from_be_bytes(prefix[8..12].try_into().expect("prefix range"));
            if block_length < MIN_RECORD_SIZE as i32 || block_length as usize > MAX_RECORD_SIZE {
                return Err(Error::Format(format!(
                    "record length is out of bounds at offset {offset}"
                )));
            }
            if block_length as u64 > remaining {
                return self.recover_incomplete_tail(offset, file_size);
            }
            let sequence = i64::from_be_bytes(prefix[12..20].try_into().expect("prefix range"));
            let block_length = block_length as usize;
            let mut block = vec![0_u8; block_length];
            block[..RECORD_PREFIX_SIZE].copy_from_slice(&prefix);
            self.file
                .read_exact(&mut block[RECORD_PREFIX_SIZE..])
                .map_err(|error| {
                    if error.kind() == std::io::ErrorKind::UnexpectedEof {
                        Error::Integrity("database changed or truncated while opening".into())
                    } else {
                        Error::Io(error)
                    }
                })?;
            let checksum_offset = block_length - format::RECORD_TRAILER_SIZE;
            let stored_checksum = u32::from_be_bytes(
                block[checksum_offset..]
                    .try_into()
                    .expect("record checksum range"),
            );
            if stored_checksum != format::crc32c(&block[..checksum_offset]) {
                return Err(Error::Integrity(format!(
                    "record checksum mismatch at offset {offset}"
                )));
            }
            if sequence != self.next_sequence {
                return Err(Error::Integrity(format!(
                    "unexpected record sequence at offset {offset}"
                )));
            }
            if self.next_sequence == i64::MAX {
                return Err(Error::Integrity(
                    "record sequence space is exhausted".into(),
                ));
            }
            let record_type = RecordType::from_code(type_code)?;
            let payload = &block[RECORD_PREFIX_SIZE..checksum_offset];
            self.replay_record(record_type, payload)?;
            self.next_sequence += 1;
            offset += block_length as u64;
        }
        Ok(())
    }

    fn replay_v2_records(&mut self, file_size: u64) -> Result<()> {
        let mut offset = HEADER_SIZE as u64;
        let mut pending: Option<PendingReplayTransaction> = None;

        while offset < file_size {
            let remaining = file_size - offset;
            if remaining < RECORD_PREFIX_SIZE as u64 {
                let truncate_at = pending
                    .as_ref()
                    .map_or(offset, |transaction| transaction.start_offset);
                let sequence = pending
                    .as_ref()
                    .map_or(self.next_sequence, |transaction| transaction.start_sequence);
                return self.recover_v2_tail(truncate_at, sequence, file_size);
            }

            self.file.seek(SeekFrom::Start(offset))?;
            let mut prefix = [0_u8; RECORD_PREFIX_SIZE];
            self.file.read_exact(&mut prefix).map_err(|error| {
                if error.kind() == std::io::ErrorKind::UnexpectedEof {
                    Error::Integrity("database changed or truncated while opening".into())
                } else {
                    Error::Io(error)
                }
            })?;
            if &prefix[..4] != RECORD_MAGIC {
                return Err(Error::Integrity(format!(
                    "invalid record magic at offset {offset}"
                )));
            }
            let type_code = prefix[4];
            if prefix[5] != 0 || prefix[6] != 0 || prefix[7] != 0 {
                return Err(Error::Format(format!(
                    "unsupported record flags at offset {offset}"
                )));
            }
            let raw_length = i32::from_be_bytes(prefix[8..12].try_into().expect("prefix range"));
            if raw_length < MIN_RECORD_SIZE as i32 || raw_length as usize > MAX_RECORD_SIZE {
                return Err(Error::Format(format!(
                    "record length is out of bounds at offset {offset}"
                )));
            }
            if raw_length as u64 > remaining {
                let truncate_at = pending
                    .as_ref()
                    .map_or(offset, |transaction| transaction.start_offset);
                let sequence = pending
                    .as_ref()
                    .map_or(self.next_sequence, |transaction| transaction.start_sequence);
                return self.recover_v2_tail(truncate_at, sequence, file_size);
            }

            let sequence = i64::from_be_bytes(prefix[12..20].try_into().expect("prefix range"));
            if sequence != self.next_sequence {
                return Err(Error::Integrity(format!(
                    "unexpected record sequence at offset {offset}"
                )));
            }
            if self.next_sequence == i64::MAX {
                return Err(Error::Integrity(
                    "record sequence space is exhausted".into(),
                ));
            }
            let block_length = raw_length as usize;
            let mut block = vec![0_u8; block_length];
            block[..RECORD_PREFIX_SIZE].copy_from_slice(&prefix);
            self.file
                .read_exact(&mut block[RECORD_PREFIX_SIZE..])
                .map_err(|error| {
                    if error.kind() == std::io::ErrorKind::UnexpectedEof {
                        Error::Integrity("database changed or truncated while opening".into())
                    } else {
                        Error::Io(error)
                    }
                })?;
            let checksum_offset = block_length - format::RECORD_TRAILER_SIZE;
            let stored_checksum = u32::from_be_bytes(
                block[checksum_offset..]
                    .try_into()
                    .expect("record checksum range"),
            );
            if stored_checksum != format::crc32c(&block[..checksum_offset]) {
                return Err(Error::Integrity(format!(
                    "record checksum mismatch at offset {offset}"
                )));
            }

            let record_type = RecordType::from_code(type_code)?;
            let payload = &block[RECORD_PREFIX_SIZE..checksum_offset];
            match record_type {
                RecordType::TransactionBegin => {
                    if pending.is_some() {
                        return Err(Error::Integrity("nested transaction begin record".into()));
                    }
                    let mut input = Cursor::new(payload);
                    let id = input.i64("transaction identifier")?;
                    input.finish("transaction begin record")?;
                    if id <= 0 || id != self.next_transaction_id {
                        return Err(Error::Integrity(
                            "invalid transaction identifier or ordering".into(),
                        ));
                    }
                    pending = Some(PendingReplayTransaction {
                        id,
                        start_offset: offset,
                        start_sequence: sequence,
                        operations: Vec::new(),
                        operation_bytes: 0,
                        index_pages: Vec::new(),
                        index_roots: Vec::new(),
                        roots_started: false,
                    });
                }
                RecordType::TransactionCommit => {
                    let mut input = Cursor::new(payload);
                    let id = input.i64("transaction identifier")?;
                    input.finish("transaction commit record")?;
                    let transaction = pending.take().ok_or_else(|| {
                        Error::Integrity("transaction commit without a begin record".into())
                    })?;
                    if id != transaction.id {
                        return Err(Error::Integrity(
                            "transaction commit identifier does not match its begin".into(),
                        ));
                    }
                    let index_batch = self.replay_index_batch(
                        id,
                        transaction.index_pages,
                        transaction.index_roots,
                    )?;
                    self.apply_replayed_transaction(id, &transaction.operations, index_batch)?;
                }
                RecordType::CollectionCreate
                | RecordType::DocumentInsert
                | RecordType::DocumentUpdate
                | RecordType::DocumentDelete => {
                    let transaction = pending.as_mut().ok_or_else(|| {
                        Error::Integrity("V2 mutation is outside a transaction".into())
                    })?;
                    if transaction.roots_started {
                        return Err(Error::Integrity(
                            "document mutation appears after index root publication".into(),
                        ));
                    }
                    let mut input = Cursor::new(payload);
                    let id = input.i64("transaction identifier")?;
                    if id != transaction.id {
                        return Err(Error::Integrity(
                            "mutation transaction identifier does not match its begin".into(),
                        ));
                    }
                    let operation_payload = &payload[8..];
                    let operation = decode_staged_operation(record_type, operation_payload)?;
                    if transaction.operations.len() >= MAX_TRANSACTION_OPERATIONS {
                        return Err(Error::Integrity(
                            "transaction operation limit exceeded in file".into(),
                        ));
                    }
                    transaction.operation_bytes = transaction
                        .operation_bytes
                        .checked_add(payload.len())
                        .ok_or_else(|| {
                            Error::Integrity("transaction payload length overflow".into())
                        })?;
                    if transaction.operation_bytes > MAX_TRANSACTION_BYTES {
                        return Err(Error::Integrity(
                            "transaction byte limit exceeded in file".into(),
                        ));
                    }
                    transaction.operations.push(operation);
                }
                RecordType::IndexPage => {
                    if self.format_version < PERSISTENT_INDEX_FORMAT_VERSION {
                        return Err(Error::Format(
                            "persistent B+Tree pages require format version 4".into(),
                        ));
                    }
                    if pending.is_none() {
                        self.replay_standalone_index_page(payload)?;
                        self.next_sequence += 1;
                        offset += block_length as u64;
                        continue;
                    }
                    let transaction = pending.as_mut().expect("pending transaction was checked");
                    if transaction.roots_started {
                        return Err(Error::Integrity(
                            "B+Tree page appears after root publication".into(),
                        ));
                    }
                    let mut input = Cursor::new(payload);
                    let transaction_id = input.i64("B+Tree transaction identifier")?;
                    if transaction_id != transaction.id {
                        return Err(Error::Integrity(
                            "B+Tree page transaction identifier does not match".into(),
                        ));
                    }
                    let page = input.bytes_exact(btree::PAGE_SIZE, "B+Tree page")?;
                    input.finish("B+Tree page record")?;
                    transaction.operation_bytes = transaction
                        .operation_bytes
                        .checked_add(payload.len())
                        .ok_or_else(|| Error::Integrity("transaction size overflow".into()))?;
                    if transaction.operation_bytes > MAX_TRANSACTION_BYTES {
                        return Err(Error::Integrity(
                            "transaction byte limit exceeded in file".into(),
                        ));
                    }
                    if transaction.index_pages.len() as u64 >= btree::MAX_BTREE_PAGES {
                        return Err(Error::Integrity(
                            "transaction B+Tree page limit exceeded in file".into(),
                        ));
                    }
                    transaction.index_pages.push(page);
                }
                RecordType::IndexRoot => {
                    if self.format_version < PERSISTENT_INDEX_FORMAT_VERSION {
                        return Err(Error::Format(
                            "transactional B+Tree roots require format version 4".into(),
                        ));
                    }
                    let transaction = pending.as_mut().ok_or_else(|| {
                        Error::Integrity("B+Tree root is outside a transaction".into())
                    })?;
                    transaction.roots_started = true;
                    let mut input = Cursor::new(payload);
                    let transaction_id = input.i64("root transaction identifier")?;
                    let index_id = input.i64("root index identifier")?;
                    let root_page_id = input.i64("root page identifier")?;
                    let generation = input.i64("root generation")?;
                    input.finish("B+Tree root publication")?;
                    if transaction_id != transaction.id
                        || index_id <= 0
                        || root_page_id <= 0
                        || generation != transaction.id
                    {
                        return Err(Error::Integrity("invalid B+Tree root publication".into()));
                    }
                    transaction.operation_bytes = transaction
                        .operation_bytes
                        .checked_add(payload.len())
                        .ok_or_else(|| Error::Integrity("transaction size overflow".into()))?;
                    if transaction.operation_bytes > MAX_TRANSACTION_BYTES {
                        return Err(Error::Integrity(
                            "transaction byte limit exceeded in file".into(),
                        ));
                    }
                    if transaction.index_roots.len() >= MAX_INDEXES {
                        return Err(Error::Integrity(
                            "transaction root publication count exceeds the index limit".into(),
                        ));
                    }
                    transaction.index_roots.push(RootPublication {
                        index_id,
                        root_page_id: root_page_id as u64,
                        generation: generation as u64,
                    });
                }
                RecordType::IndexCreate => {
                    if pending.is_some() {
                        return Err(Error::Integrity(
                            "index definition appears inside a transaction".into(),
                        ));
                    }
                    self.replay_index_definition(payload)?;
                }
                RecordType::DocumentIdWatermark => {
                    if pending.is_some() {
                        return Err(Error::Integrity(
                            "document-ID watermark appears inside a transaction".into(),
                        ));
                    }
                    self.replay_document_id_watermark(payload)?;
                }
                RecordType::VersionWatermark => {
                    if self.format_version < MVCC_FORMAT_VERSION || pending.is_some() {
                        return Err(Error::Integrity(
                            "version watermark is invalid in this file or transaction".into(),
                        ));
                    }
                    self.replay_version_watermark(payload)?;
                }
            }

            self.next_sequence += 1;
            offset += block_length as u64;
        }

        if let Some(transaction) = pending {
            return self.recover_v2_tail(
                transaction.start_offset,
                transaction.start_sequence,
                file_size,
            );
        }
        Ok(())
    }

    fn apply_replayed_transaction(
        &mut self,
        id: i64,
        operations: &[StagedOperation],
        index_batch: PendingIndexBatch,
    ) -> Result<()> {
        if id != self.next_transaction_id {
            return Err(Error::Integrity(
                "committed transaction identifier is out of order".into(),
            ));
        }
        let mut transaction = Transaction::new(self, id);
        for operation in operations {
            let result = match operation {
                StagedOperation::CollectionCreate {
                    id: expected_id,
                    name,
                } => transaction.create_collection(name).and_then(|actual| {
                    if actual.value() == *expected_id {
                        Ok(())
                    } else {
                        Err(Error::Integrity(
                            "collection identifier is not sequential".into(),
                        ))
                    }
                }),
                StagedOperation::DocumentInsert {
                    collection_id,
                    id,
                    document,
                } => transaction
                    .collection_name(*collection_id)
                    .and_then(|name| transaction.insert_with_id(&name, *id, document.clone())),
                StagedOperation::DocumentUpdate {
                    collection_id,
                    id,
                    document,
                } => transaction
                    .collection_name(*collection_id)
                    .and_then(|name| {
                        if transaction.update(&name, *id, document.clone())? {
                            Ok(())
                        } else {
                            Err(Error::Integrity(
                                "update record names an unknown document".into(),
                            ))
                        }
                    }),
                StagedOperation::DocumentDelete { collection_id, id } => transaction
                    .collection_name(*collection_id)
                    .and_then(|name| {
                        if transaction.delete(&name, *id)? {
                            Ok(())
                        } else {
                            Err(Error::Integrity(
                                "delete record names an unknown document".into(),
                            ))
                        }
                    }),
            };
            result.map_err(|error| {
                Error::Integrity(format!("invalid committed transaction: {error}"))
            })?;
        }
        transaction
            .apply_staged_with_index_batch(Some(index_batch))
            .map_err(|error| Error::Integrity(format!("could not replay transaction: {error}")))
    }

    fn replay_standalone_index_page(&mut self, payload: &[u8]) -> Result<()> {
        let (page_id, owner, generation) = btree::decode_stored_page(payload)?;
        if payload.len() != btree::PAGE_SIZE
            || page_id != self.next_page_id
            || owner != self.next_index_id
            || generation > self.next_transaction_id.saturating_sub(1) as u64
            || self.persistent_pages.len() as u64 >= btree::MAX_BTREE_PAGES
        {
            return Err(Error::Integrity(
                "standalone B+Tree page has invalid ID, owner, generation or size".into(),
            ));
        }
        self.persistent_pages.insert(page_id, payload.to_vec());
        self.next_page_id = page_id
            .checked_add(1)
            .ok_or_else(|| Error::Integrity("B+Tree page ID overflow".into()))?;
        Ok(())
    }

    fn replay_index_batch(
        &self,
        transaction_id: i64,
        pages: Vec<Vec<u8>>,
        roots: Vec<RootPublication>,
    ) -> Result<PendingIndexBatch> {
        if self.format_version < PERSISTENT_INDEX_FORMAT_VERSION {
            if !pages.is_empty() || !roots.is_empty() {
                return Err(Error::Integrity(
                    "legacy transaction contains persistent B+Tree records".into(),
                ));
            }
            return Ok(PendingIndexBatch::default());
        }
        if pages.len() as u64 > btree::MAX_BTREE_PAGES {
            return Err(Error::Integrity(
                "transaction B+Tree page limit exceeded".into(),
            ));
        }
        let mut batch = PendingIndexBatch {
            next_page_id: self.next_page_id,
            ..PendingIndexBatch::default()
        };
        for bytes in pages {
            let (page_id, owner, generation) = btree::decode_stored_page(&bytes)?;
            if page_id != batch.next_page_id
                || generation != transaction_id as u64
                || !self.secondary_indexes.contains_key(&owner)
            {
                return Err(Error::Integrity(
                    "transaction B+Tree page has invalid identity or generation".into(),
                ));
            }
            batch.pages.insert(page_id, bytes);
            batch.next_page_id = batch
                .next_page_id
                .checked_add(1)
                .ok_or_else(|| Error::Integrity("B+Tree page ID overflow".into()))?;
        }
        batch.roots = roots;
        Ok(batch)
    }

    fn replay_index_definition(&mut self, payload: &[u8]) -> Result<()> {
        let mut input = Cursor::new(payload);
        let index_id = input.i64("index identifier")?;
        let collection_id = input.i64("index collection identifier")?;
        let unique = match input.u8("index unique flag")? {
            0 => false,
            1 => true,
            _ => return Err(Error::Integrity("invalid index unique flag".into())),
        };
        let segment_count = input.i32("field path segment count")?;
        if segment_count <= 0 || segment_count as usize > crate::MAX_FIELD_PATH_DEPTH {
            return Err(Error::Integrity(
                "field path segment count is out of bounds".into(),
            ));
        }
        let mut segments = Vec::with_capacity(segment_count as usize);
        for _ in 0..segment_count {
            segments.push(input.string(1_024, "field path segment")?);
        }
        let (root_page_id, root_generation) =
            if self.format_version >= PERSISTENT_INDEX_FORMAT_VERSION {
                let root_page_id = input.i64("index root page identifier")?;
                let root_generation = input.i64("index root generation")?;
                if root_page_id <= 0 || root_generation < 0 {
                    return Err(Error::Integrity("invalid persistent index root".into()));
                }
                (root_page_id as u64, root_generation as u64)
            } else {
                (0, 0)
            };
        input.finish("index definition record")?;
        if index_id <= 0 || index_id != self.next_index_id {
            return Err(Error::Integrity(
                "index identifiers must be positive and sequential".into(),
            ));
        }
        if !self.collection_names_by_id.contains_key(&collection_id) {
            return Err(Error::Integrity(
                "index definition names an unknown collection".into(),
            ));
        }
        if self.secondary_indexes.len() >= MAX_INDEXES {
            return Err(Error::Integrity(
                "index count limit exceeded in file".into(),
            ));
        }
        let path = FieldPath::new(segments)
            .map_err(|_| Error::Integrity("invalid field path in index definition".into()))?;
        if self
            .secondary_indexes
            .values()
            .any(|existing| existing.collection_id == collection_id && existing.path == path)
        {
            return Err(Error::Integrity(
                "duplicate field path in index definitions".into(),
            ));
        }
        let mut index = SecondaryIndex::new(
            IndexId::from_positive(index_id),
            collection_id,
            path,
            unique,
        );
        let collection_name = self
            .collection_names_by_id
            .get(&collection_id)
            .expect("collection identifier was checked");
        let collection = self
            .collections
            .get(collection_name)
            .expect("collection name map was checked");
        if self.format_version >= PERSISTENT_INDEX_FORMAT_VERSION {
            if root_generation > self.next_transaction_id.saturating_sub(1) as u64 {
                return Err(Error::Integrity(
                    "persistent index root generation is in the future".into(),
                ));
            }
            let tree_entries = btree::collect_entries(
                root_page_id,
                index_id,
                root_generation,
                &self.persistent_pages,
            )?;
            for entry in tree_entries {
                let document = collection
                    .documents
                    .get(&entry.document_id.value())
                    .ok_or_else(|| {
                        Error::Integrity("persistent index references a missing document".into())
                    })?;
                if index.key_for(document)?.as_ref() != Some(&entry.value) {
                    return Err(Error::Integrity(
                        "persistent index key does not match its document".into(),
                    ));
                }
                index.insert(entry.document_id, document).map_err(|error| {
                    Error::Integrity(format!("invalid persisted index: {error}"))
                })?;
            }
            index.root_page_id = root_page_id;
            index.root_generation = root_generation;
        } else {
            for (document_id, document) in &collection.documents {
                index
                    .insert(DocumentId(*document_id), document)
                    .map_err(|error| {
                        Error::Integrity(format!("invalid persisted index: {error}"))
                    })?;
            }
        }
        self.secondary_indexes.insert(index_id, index);
        self.next_index_id = index_id.checked_add(1).unwrap_or(0);
        Ok(())
    }

    fn replay_document_id_watermark(&mut self, payload: &[u8]) -> Result<()> {
        let mut input = Cursor::new(payload);
        let next_id = input.i64("next document identifier")?;
        let exhausted = match input.u8("document identifier exhaustion flag")? {
            0 => false,
            1 => true,
            _ => {
                return Err(Error::Integrity(
                    "invalid document identifier exhaustion flag".into(),
                ));
            }
        };
        input.finish("document-ID watermark record")?;
        if next_id <= 0
            || (!exhausted && next_id < self.next_document_id)
            || (self.document_ids_exhausted && !exhausted)
        {
            return Err(Error::Integrity(
                "document-ID watermark moves the identifier state backwards".into(),
            ));
        }
        self.next_document_id = next_id;
        self.document_ids_exhausted = exhausted;
        Ok(())
    }

    fn replay_version_watermark(&mut self, payload: &[u8]) -> Result<()> {
        let mut input = Cursor::new(payload);
        let committed_version = input.i64("committed version watermark")?;
        input.finish("version watermark record")?;
        let current = self.next_transaction_id.saturating_sub(1);
        if committed_version < current || committed_version <= 0 || committed_version == i64::MAX {
            return Err(Error::Integrity(
                "version watermark is invalid or moves backwards".into(),
            ));
        }
        self.next_transaction_id = committed_version + 1;
        Ok(())
    }

    fn recover_v2_tail(
        &mut self,
        last_good_offset: u64,
        sequence: i64,
        original_size: u64,
    ) -> Result<()> {
        if !self.writable {
            return Err(Error::RecoveryRequired);
        }
        self.file.set_len(last_good_offset)?;
        crash_pause_if("recovery_after_truncate_before_sync");
        self.file.sync_all()?;
        crash_pause_if("recovery_after_sync");
        self.next_sequence = sequence;
        self.recovery_report = RecoveryReport {
            discarded_bytes: original_size - last_good_offset,
        };
        Ok(())
    }

    fn replay_record(&mut self, record_type: RecordType, payload: &[u8]) -> Result<()> {
        let mut input = Cursor::new(payload);
        match record_type {
            RecordType::CollectionCreate => {
                let collection_id = input.i64("collection identifier")?;
                if collection_id <= 0 || collection_id != self.next_collection_id {
                    return Err(Error::Integrity(
                        "invalid collection identifier in record".into(),
                    ));
                }
                let name = input.string(COLLECTION_NAME_MAX_BYTES, "collection name")?;
                validate_collection_name(&name)
                    .map_err(|_| Error::Integrity("invalid collection name in record".into()))?;
                input.finish("collection record")?;
                if self.collections.contains_key(&name) || self.collections.len() >= MAX_COLLECTIONS
                {
                    return Err(Error::Integrity(
                        "duplicate collection or collection limit exceeded".into(),
                    ));
                }
                self.collections.insert(
                    name.clone(),
                    CollectionState {
                        id: collection_id,
                        documents: BTreeMap::new(),
                    },
                );
                self.collection_names_by_id.insert(collection_id, name);
                self.next_collection_id = collection_id.checked_add(1).unwrap_or(0);
            }
            RecordType::DocumentInsert | RecordType::DocumentUpdate => {
                let (collection_id, document_id, document) = decode_document_mutation(&mut input)?;
                let name = self
                    .collection_names_by_id
                    .get(&collection_id)
                    .ok_or_else(|| {
                        Error::Integrity("document record names an unknown collection".into())
                    })?
                    .clone();
                let current_owner = self.primary_id_index.get(&document_id).copied();
                if record_type == RecordType::DocumentInsert {
                    if current_owner.is_some() {
                        return Err(Error::Integrity("duplicate document ID in file".into()));
                    }
                    if self.document_count >= MAX_DOCUMENTS {
                        return Err(Error::Integrity(
                            "active document limit exceeded in file".into(),
                        ));
                    }
                    self.collections
                        .get_mut(&name)
                        .expect("collection map checked")
                        .documents
                        .insert(document_id, document);
                    self.primary_id_index.insert(document_id, collection_id);
                    self.document_count += 1;
                    self.advance_next_document_id(document_id);
                } else {
                    if current_owner != Some(collection_id) {
                        return Err(Error::Integrity(
                            "update record names an unknown document".into(),
                        ));
                    }
                    self.collections
                        .get_mut(&name)
                        .expect("collection map checked")
                        .documents
                        .insert(document_id, document);
                }
            }
            RecordType::DocumentDelete => {
                let collection_id = input.i64("delete collection identifier")?;
                let document_id = input.i64("delete document identifier")?;
                input.finish("delete record")?;
                if document_id <= 0
                    || self.primary_id_index.get(&document_id) != Some(&collection_id)
                {
                    return Err(Error::Integrity(
                        "delete record names an unknown document".into(),
                    ));
                }
                let name = self
                    .collection_names_by_id
                    .get(&collection_id)
                    .ok_or_else(|| {
                        Error::Integrity("delete record names an unknown collection".into())
                    })?
                    .clone();
                self.collections
                    .get_mut(&name)
                    .expect("collection map checked")
                    .documents
                    .remove(&document_id);
                self.primary_id_index.remove(&document_id);
                self.document_count -= 1;
            }
            RecordType::TransactionBegin
            | RecordType::TransactionCommit
            | RecordType::IndexCreate
            | RecordType::IndexPage
            | RecordType::IndexRoot
            | RecordType::DocumentIdWatermark
            | RecordType::VersionWatermark => {
                return Err(Error::Format(
                    "version 2 record type cannot appear in a version 1 database".into(),
                ));
            }
        }
        Ok(())
    }

    fn recover_incomplete_tail(&mut self, last_good_offset: u64, original_size: u64) -> Result<()> {
        if !self.writable {
            return Err(Error::RecoveryRequired);
        }
        self.file.set_len(last_good_offset)?;
        self.file.sync_all()?;
        self.recovery_report = RecoveryReport {
            discarded_bytes: original_size - last_good_offset,
        };
        Ok(())
    }

    fn append_record(&mut self, record_type: RecordType, payload: &[u8]) -> Result<()> {
        self.check_writable()?;
        if self.next_sequence <= 0 || self.next_sequence == i64::MAX {
            return Err(Error::Limit("record sequence space is exhausted".into()));
        }
        let total_length = MIN_RECORD_SIZE
            .checked_add(payload.len())
            .ok_or_else(|| Error::Limit("record length overflow".into()))?;
        if total_length > MAX_RECORD_SIZE {
            return Err(Error::Limit(
                "record exceeds the 17 MiB maximum block size".into(),
            ));
        }
        let current_size = self.file.metadata()?.len();
        if current_size
            .checked_add(total_length as u64)
            .is_none_or(|size| size > MAX_DATABASE_BYTES)
        {
            return Err(Error::Limit(
                "database file size limit reached; write a compacted copy and switch files manually".into(),
            ));
        }
        let block = encode_record(record_type, self.next_sequence, payload)?;
        self.file.lock()?;
        if let Err(error) = self.file.seek(SeekFrom::End(0)) {
            let _ = self.file.unlock();
            return Err(Error::Io(error));
        }
        let write_result = (|| -> std::io::Result<()> {
            crash_before_write();
            if crash_during_write() {
                self.file.write_all(&block[..(block.len() / 2).max(1)])?;
                crash_pause("during_write");
            }
            self.file.write_all(&block)?;
            crash_after_write();
            self.file.sync_all()?;
            crash_after_sync();
            Ok(())
        })();
        let unlock_result = self.file.unlock();
        if let Err(error) = write_result {
            self.write_failed = true;
            return Err(Error::Io(error));
        }
        if let Err(error) = unlock_result {
            self.write_failed = true;
            return Err(Error::Io(error));
        }
        self.next_sequence += 1;
        Ok(())
    }

    fn collection_id(&self, name: &str) -> Result<i64> {
        validate_collection_name(name)?;
        self.collections
            .get(name)
            .map(|state| state.id)
            .ok_or_else(|| Error::NotFound(format!("collection {name:?}")))
    }

    fn collection(&self, name: &str) -> Result<&CollectionState> {
        validate_collection_name(name)?;
        self.collections
            .get(name)
            .ok_or_else(|| Error::NotFound(format!("collection {name:?}")))
    }

    fn advance_next_document_id(&mut self, used_id: i64) {
        if used_id >= self.next_document_id {
            if used_id == i64::MAX {
                self.document_ids_exhausted = true;
            } else {
                self.next_document_id = used_id + 1;
            }
        }
    }

    fn check_usable(&self) -> Result<()> {
        if self.write_failed {
            return Err(Error::Io(std::io::Error::other(
                "a prior append failed; close and reopen the database",
            )));
        }
        Ok(())
    }

    fn check_writable(&self) -> Result<()> {
        if !self.writable {
            return Err(Error::ReadOnly);
        }
        Ok(())
    }
}

/// A staged V2/V3 transaction. Changes become visible only after its commit marker syncs.
pub struct Transaction<'database> {
    database: &'database mut Database,
    id: i64,
    operations: Vec<StagedOperation>,
    overlay: HashMap<i64, OverlayDocument>,
    new_collections: HashMap<String, i64>,
    next_collection_id: i64,
    next_document_id: i64,
    document_ids_exhausted: bool,
    operation_bytes: usize,
}

struct PendingReplayTransaction {
    id: i64,
    start_offset: u64,
    start_sequence: i64,
    operations: Vec<StagedOperation>,
    operation_bytes: usize,
    index_pages: Vec<Vec<u8>>,
    index_roots: Vec<RootPublication>,
    roots_started: bool,
}

#[derive(Default)]
struct PendingIndexBatch {
    pages: BTreeMap<u64, Vec<u8>>,
    roots: Vec<RootPublication>,
    next_page_id: u64,
}

struct RootPublication {
    index_id: i64,
    root_page_id: u64,
    generation: u64,
}

struct IndexDelta {
    index_id: i64,
    document_id: DocumentId,
    old_key: Option<IndexValue>,
    new_key: Option<IndexValue>,
}

struct OverlayDocument {
    collection_id: i64,
    document: Option<Document>,
}

enum StagedOperation {
    CollectionCreate {
        id: i64,
        name: String,
    },
    DocumentInsert {
        collection_id: i64,
        id: DocumentId,
        document: Document,
    },
    DocumentUpdate {
        collection_id: i64,
        id: DocumentId,
        document: Document,
    },
    DocumentDelete {
        collection_id: i64,
        id: DocumentId,
    },
}

impl StagedOperation {
    fn record_type(&self) -> RecordType {
        match self {
            Self::CollectionCreate { .. } => RecordType::CollectionCreate,
            Self::DocumentInsert { .. } => RecordType::DocumentInsert,
            Self::DocumentUpdate { .. } => RecordType::DocumentUpdate,
            Self::DocumentDelete { .. } => RecordType::DocumentDelete,
        }
    }

    fn payload(&self) -> Result<Vec<u8>> {
        match self {
            Self::CollectionCreate { id, name } => encode_collection_create(*id, name),
            Self::DocumentInsert {
                collection_id,
                id,
                document,
            }
            | Self::DocumentUpdate {
                collection_id,
                id,
                document,
            } => encode_document_mutation(*collection_id, *id, document),
            Self::DocumentDelete { collection_id, id } => {
                let mut payload = Vec::with_capacity(16);
                payload.extend_from_slice(&collection_id.to_be_bytes());
                payload.extend_from_slice(&id.value().to_be_bytes());
                Ok(payload)
            }
        }
    }
}

fn decode_staged_operation(record_type: RecordType, payload: &[u8]) -> Result<StagedOperation> {
    let mut input = Cursor::new(payload);
    match record_type {
        RecordType::CollectionCreate => {
            let id = input.i64("collection identifier")?;
            let name = input.string(COLLECTION_NAME_MAX_BYTES, "collection name")?;
            input.finish("collection record")?;
            if id <= 0 || validate_collection_name(&name).is_err() {
                return Err(Error::Integrity(
                    "invalid collection creation in transaction".into(),
                ));
            }
            Ok(StagedOperation::CollectionCreate { id, name })
        }
        RecordType::DocumentInsert | RecordType::DocumentUpdate => {
            let (collection_id, id, document) = decode_document_mutation(&mut input)?;
            let id = DocumentId::new(id)
                .map_err(|_| Error::Integrity("document ID must be positive".into()))?;
            if record_type == RecordType::DocumentInsert {
                Ok(StagedOperation::DocumentInsert {
                    collection_id,
                    id,
                    document,
                })
            } else {
                Ok(StagedOperation::DocumentUpdate {
                    collection_id,
                    id,
                    document,
                })
            }
        }
        RecordType::DocumentDelete => {
            let collection_id = input.i64("delete collection identifier")?;
            let id = input.i64("delete document identifier")?;
            input.finish("delete record")?;
            if collection_id <= 0 || id <= 0 {
                return Err(Error::Integrity(
                    "delete identifiers must be positive".into(),
                ));
            }
            Ok(StagedOperation::DocumentDelete {
                collection_id,
                id: DocumentId(id),
            })
        }
        RecordType::TransactionBegin
        | RecordType::TransactionCommit
        | RecordType::IndexCreate
        | RecordType::IndexPage
        | RecordType::IndexRoot
        | RecordType::DocumentIdWatermark
        | RecordType::VersionWatermark => Err(Error::Integrity(
            "non-mutation record appeared in a transaction".into(),
        )),
    }
}

impl<'database> Transaction<'database> {
    fn new(database: &'database mut Database, id: i64) -> Self {
        Self {
            next_collection_id: database.next_collection_id,
            next_document_id: database.next_document_id,
            document_ids_exhausted: database.document_ids_exhausted,
            database,
            id,
            operations: Vec::new(),
            overlay: HashMap::new(),
            new_collections: HashMap::new(),
            operation_bytes: 0,
        }
    }

    /// Persists a new collection as part of this transaction.
    pub fn create_collection(&mut self, name: &str) -> Result<CollectionId> {
        self.database.check_usable()?;
        validate_collection_name(name)?;
        if self.database.collections.contains_key(name) || self.new_collections.contains_key(name) {
            return Err(Error::Duplicate(format!(
                "collection {name:?} already exists"
            )));
        }
        if self.database.collections.len() + self.new_collections.len() >= MAX_COLLECTIONS {
            return Err(Error::Limit("collection count reached 10,000".into()));
        }
        if self.next_collection_id <= 0 || self.next_collection_id == i64::MAX {
            return Err(Error::Limit(
                "collection identifier space is exhausted".into(),
            ));
        }
        let id = self.next_collection_id;
        let operation = StagedOperation::CollectionCreate {
            id,
            name: name.to_owned(),
        };
        self.push_operation(&operation)?;
        self.new_collections.insert(name.to_owned(), id);
        self.next_collection_id += 1;
        self.operations.push(operation);
        Ok(CollectionId(id))
    }

    /// Inserts a document and returns a provisional ID, valid if commit succeeds.
    pub fn insert(&mut self, collection_name: &str, document: Document) -> Result<DocumentId> {
        self.database.check_usable()?;
        if self.document_ids_exhausted || self.next_document_id <= 0 {
            return Err(Error::Limit(
                "document identifier space is exhausted".into(),
            ));
        }
        let id = DocumentId(self.next_document_id);
        self.insert_with_id(collection_name, id, document)?;
        Ok(id)
    }

    /// Inserts under a caller-selected positive ID as one staged operation.
    pub fn insert_with_id(
        &mut self,
        collection_name: &str,
        id: DocumentId,
        document: Document,
    ) -> Result<()> {
        self.database.check_usable()?;
        let collection_id = self.collection_id(collection_name)?;
        if self.effective_document(id.value()).is_some() {
            return Err(Error::Duplicate(format!("document ID {}", id.value())));
        }
        if self.projected_document_count() >= MAX_DOCUMENTS {
            return Err(Error::Limit("active document count reached 100,000".into()));
        }
        self.validate_index_values(collection_id, &document)?;
        self.check_unique_values(collection_id, id, &document)?;
        let operation = StagedOperation::DocumentInsert {
            collection_id,
            id,
            document: document.clone(),
        };
        self.push_operation(&operation)?;
        self.overlay.insert(
            id.value(),
            OverlayDocument {
                collection_id,
                document: Some(document),
            },
        );
        self.advance_document_id(id.value());
        self.operations.push(operation);
        Ok(())
    }

    /// Replaces a document visible in this transaction.
    pub fn update(
        &mut self,
        collection_name: &str,
        id: DocumentId,
        document: Document,
    ) -> Result<bool> {
        self.database.check_usable()?;
        let collection_id = self.collection_id(collection_name)?;
        if self
            .effective_document(id.value())
            .is_none_or(|(owner, _)| owner != collection_id)
        {
            return Ok(false);
        }
        self.validate_index_values(collection_id, &document)?;
        self.check_unique_values(collection_id, id, &document)?;
        let operation = StagedOperation::DocumentUpdate {
            collection_id,
            id,
            document: document.clone(),
        };
        self.push_operation(&operation)?;
        self.overlay.insert(
            id.value(),
            OverlayDocument {
                collection_id,
                document: Some(document),
            },
        );
        self.operations.push(operation);
        Ok(true)
    }

    /// Applies document operations as one staged replacement.
    pub fn update_fields(
        &mut self,
        collection_name: &str,
        id: DocumentId,
        operations: &[crate::DocumentOperation],
    ) -> Result<bool> {
        let Some(document) = self.get(collection_name, id)? else {
            return Ok(false);
        };
        let updated = crate::apply_document_operations(document, operations)?;
        self.update(collection_name, id, updated)
    }

    /// Deletes a document visible in this transaction.
    pub fn delete(&mut self, collection_name: &str, id: DocumentId) -> Result<bool> {
        self.database.check_usable()?;
        let collection_id = self.collection_id(collection_name)?;
        if self
            .effective_document(id.value())
            .is_none_or(|(owner, _)| owner != collection_id)
        {
            return Ok(false);
        }
        let operation = StagedOperation::DocumentDelete { collection_id, id };
        self.push_operation(&operation)?;
        self.overlay.insert(
            id.value(),
            OverlayDocument {
                collection_id,
                document: None,
            },
        );
        self.operations.push(operation);
        Ok(true)
    }

    /// Reads the transaction's own staged view, including inserts and deletes.
    pub fn get(&self, collection_name: &str, id: DocumentId) -> Result<Option<&Document>> {
        let collection_id = self.collection_id(collection_name)?;
        Ok(self
            .effective_document(id.value())
            .filter(|(owner, _)| *owner == collection_id)
            .map(|(_, document)| document))
    }

    /// Makes all staged mutations visible after one commit marker and file sync.
    pub fn commit(self) -> Result<()> {
        let index_batch =
            self.database
                .commit_transaction(self.id, &self.operations, &self.overlay)?;
        self.apply_staged_with_index_batch(Some(index_batch))
    }

    /// Discards all staged changes. Provisional IDs can be reused after rollback.
    pub fn rollback(self) {}

    fn apply_staged_with_index_batch(self, index_batch: Option<PendingIndexBatch>) -> Result<()> {
        let Transaction {
            database,
            id,
            operations,
            overlay,
            next_collection_id,
            next_document_id,
            document_ids_exhausted,
            ..
        } = self;
        let result = database.apply_transaction_state(
            id,
            &operations,
            &overlay,
            next_collection_id,
            next_document_id,
            document_ids_exhausted,
        );
        let result = result.and_then(|()| {
            if let Some(batch) = index_batch {
                database.publish_index_batch(id, batch)?;
            }
            Ok(())
        });
        if result.is_err() {
            database.write_failed = true;
        }
        result
    }

    fn push_operation(&mut self, operation: &StagedOperation) -> Result<()> {
        if self.operations.len() >= MAX_TRANSACTION_OPERATIONS {
            return Err(Error::Limit(format!(
                "transaction exceeds {MAX_TRANSACTION_OPERATIONS} operations"
            )));
        }
        let encoded_size = operation
            .payload()?
            .len()
            .checked_add(8)
            .ok_or_else(|| Error::Limit("transaction payload length overflow".into()))?;
        if encoded_size + MIN_RECORD_SIZE > MAX_RECORD_SIZE {
            return Err(Error::Limit(
                "transaction operation exceeds the 17 MiB record limit".into(),
            ));
        }
        self.operation_bytes = self
            .operation_bytes
            .checked_add(encoded_size)
            .ok_or_else(|| Error::Limit("transaction size overflow".into()))?;
        if self.operation_bytes > MAX_TRANSACTION_BYTES {
            self.operation_bytes -= encoded_size;
            return Err(Error::Limit(format!(
                "transaction exceeds the {MAX_TRANSACTION_BYTES}-byte operation limit"
            )));
        }
        Ok(())
    }

    fn collection_id(&self, name: &str) -> Result<i64> {
        validate_collection_name(name)?;
        self.new_collections
            .get(name)
            .copied()
            .or_else(|| self.database.collections.get(name).map(|state| state.id))
            .ok_or_else(|| Error::NotFound(format!("collection {name:?}")))
    }

    fn collection_name(&self, id: i64) -> Result<String> {
        self.database
            .collection_names_by_id
            .get(&id)
            .cloned()
            .or_else(|| {
                self.new_collections
                    .iter()
                    .find_map(|(name, candidate)| (*candidate == id).then(|| name.clone()))
            })
            .ok_or_else(|| Error::Integrity("transaction names an unknown collection".into()))
    }

    fn effective_document(&self, id: i64) -> Option<(i64, &Document)> {
        if let Some(overlay) = self.overlay.get(&id) {
            return overlay
                .document
                .as_ref()
                .map(|document| (overlay.collection_id, document));
        }
        let collection_id = *self.database.primary_id_index.get(&id)?;
        let collection_name = self.database.collection_names_by_id.get(&collection_id)?;
        let document = self
            .database
            .collections
            .get(collection_name)?
            .documents
            .get(&id)?;
        Some((collection_id, document))
    }

    fn projected_document_count(&self) -> usize {
        let mut count = self.database.document_count;
        for (id, entry) in &self.overlay {
            let existed = self.database.primary_id_index.contains_key(id);
            match (existed, entry.document.is_some()) {
                (true, false) => count = count.saturating_sub(1),
                (false, true) => count = count.saturating_add(1),
                _ => {}
            }
        }
        count
    }

    fn validate_index_values(&self, collection_id: i64, document: &Document) -> Result<()> {
        for index in self
            .database
            .secondary_indexes
            .values()
            .filter(|index| index.collection_id == collection_id)
        {
            index.key_for(document)?;
        }
        Ok(())
    }

    fn check_unique_values(
        &self,
        collection_id: i64,
        id: DocumentId,
        document: &Document,
    ) -> Result<()> {
        for index in self
            .database
            .secondary_indexes
            .values()
            .filter(|index| index.collection_id == collection_id && index.unique)
        {
            let Some(key) = index.key_for(document)? else {
                continue;
            };
            if let Some(indexed_ids) = index.entries.get(&key) {
                for existing_id in indexed_ids {
                    if *existing_id != id
                        && self.effective_document(existing_id.value()).is_some_and(
                            |(owner, existing)| {
                                owner == collection_id
                                    && index
                                        .key_for(existing)
                                        .is_ok_and(|value| value.as_ref() == Some(&key))
                            },
                        )
                    {
                        return Err(Error::ConstraintViolation(format!(
                            "unique index {} rejects duplicate value",
                            index.path
                        )));
                    }
                }
            }
            for (existing_id, overlay) in &self.overlay {
                if *existing_id == id.value() || overlay.collection_id != collection_id {
                    continue;
                }
                if let Some(existing) = &overlay.document
                    && index.key_for(existing)?.as_ref() == Some(&key)
                {
                    return Err(Error::ConstraintViolation(format!(
                        "unique index {} rejects duplicate value",
                        index.path
                    )));
                }
            }
        }
        Ok(())
    }

    fn advance_document_id(&mut self, used: i64) {
        if used >= self.next_document_id {
            if used == i64::MAX {
                self.document_ids_exhausted = true;
            } else {
                self.next_document_id = used + 1;
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecordType {
    CollectionCreate,
    DocumentInsert,
    DocumentUpdate,
    DocumentDelete,
    TransactionBegin,
    TransactionCommit,
    IndexCreate,
    DocumentIdWatermark,
    VersionWatermark,
    IndexPage,
    IndexRoot,
}

fn equality_terms(filter: &Filter) -> Vec<(&FieldPath, &Value)> {
    fn collect<'a>(filter: &'a Filter, values: &mut Vec<(&'a FieldPath, &'a Value)>) {
        match filter {
            Filter::Compare {
                path,
                op: CompareOp::Eq,
                value,
            } => values.push((path, value)),
            Filter::And(children) => {
                for child in children {
                    collect(child, values);
                }
            }
            _ => {}
        }
    }
    let mut values = Vec::new();
    collect(filter, &mut values);
    values
}

fn range_terms(filter: &Filter) -> Vec<(&FieldPath, CompareOp, &Value)> {
    fn collect<'a>(filter: &'a Filter, values: &mut Vec<(&'a FieldPath, CompareOp, &'a Value)>) {
        match filter {
            Filter::Compare { path, op, value }
                if matches!(
                    op,
                    CompareOp::Lt | CompareOp::Lte | CompareOp::Gt | CompareOp::Gte
                ) =>
            {
                values.push((path, *op, value));
            }
            Filter::And(children) => {
                for child in children {
                    collect(child, values);
                }
            }
            _ => {}
        }
    }
    let mut values = Vec::new();
    collect(filter, &mut values);
    values
}

fn index_range_candidates(
    index: &SecondaryIndex,
    key: &IndexValue,
    operator: CompareOp,
) -> Vec<DocumentId> {
    use std::ops::Bound::{Excluded, Unbounded};

    match operator {
        CompareOp::Lt => index
            .entries
            .range(..key.clone())
            .flat_map(|(_, ids)| ids.iter().copied())
            .collect(),
        CompareOp::Lte => index
            .entries
            .range(..=key.clone())
            .flat_map(|(_, ids)| ids.iter().copied())
            .collect(),
        CompareOp::Gt => index
            .entries
            .range((Excluded(key.clone()), Unbounded))
            .flat_map(|(_, ids)| ids.iter().copied())
            .collect(),
        CompareOp::Gte => index
            .entries
            .range(key.clone()..)
            .flat_map(|(_, ids)| ids.iter().copied())
            .collect(),
        CompareOp::Eq | CompareOp::Ne => Vec::new(),
    }
}

impl RecordType {
    const fn code(self) -> u8 {
        match self {
            Self::CollectionCreate => 1,
            Self::DocumentInsert => 2,
            Self::DocumentUpdate => 3,
            Self::DocumentDelete => 4,
            Self::TransactionBegin => 5,
            Self::TransactionCommit => 6,
            Self::IndexCreate => 7,
            Self::DocumentIdWatermark => 8,
            Self::VersionWatermark => 9,
            Self::IndexPage => 10,
            Self::IndexRoot => 11,
        }
    }

    fn from_code(code: u8) -> Result<Self> {
        match code {
            1 => Ok(Self::CollectionCreate),
            2 => Ok(Self::DocumentInsert),
            3 => Ok(Self::DocumentUpdate),
            4 => Ok(Self::DocumentDelete),
            5 => Ok(Self::TransactionBegin),
            6 => Ok(Self::TransactionCommit),
            7 => Ok(Self::IndexCreate),
            8 => Ok(Self::DocumentIdWatermark),
            9 => Ok(Self::VersionWatermark),
            10 => Ok(Self::IndexPage),
            11 => Ok(Self::IndexRoot),
            _ => Err(Error::Format(format!("unknown record type {code}"))),
        }
    }
}

fn encode_record(record_type: RecordType, sequence: i64, payload: &[u8]) -> Result<Vec<u8>> {
    let total_length = MIN_RECORD_SIZE
        .checked_add(payload.len())
        .ok_or_else(|| Error::Limit("record length overflow".into()))?;
    if total_length > MAX_RECORD_SIZE {
        return Err(Error::Limit("record exceeds the maximum block size".into()));
    }
    let mut block = Vec::with_capacity(total_length);
    block.extend_from_slice(RECORD_MAGIC);
    block.push(record_type.code());
    block.push(0);
    block.extend_from_slice(&0_u16.to_be_bytes());
    block.extend_from_slice(&(total_length as i32).to_be_bytes());
    block.extend_from_slice(&sequence.to_be_bytes());
    block.extend_from_slice(payload);
    let checksum = format::crc32c(&block);
    block.extend_from_slice(&checksum.to_be_bytes());
    Ok(block)
}

fn write_snapshot_record(
    file: &mut File,
    sequence: &mut i64,
    file_size: &mut u64,
    record_type: RecordType,
    payload: &[u8],
) -> Result<()> {
    if *sequence <= 0 || *sequence == i64::MAX {
        return Err(Error::Limit("record sequence space is exhausted".into()));
    }
    let block = encode_record(record_type, *sequence, payload)?;
    let updated_size = file_size
        .checked_add(block.len() as u64)
        .ok_or_else(|| Error::Limit("database file length overflow".into()))?;
    if updated_size > MAX_DATABASE_BYTES {
        return Err(Error::Limit(
            "compacted database exceeds the 128 MiB file limit".into(),
        ));
    }
    file.write_all(&block)?;
    *file_size = updated_size;
    *sequence += 1;
    Ok(())
}

fn encode_collection_create(collection_id: i64, name: &str) -> Result<Vec<u8>> {
    let name_bytes = name.as_bytes();
    if name_bytes.len() > COLLECTION_NAME_MAX_BYTES {
        return Err(Error::Limit("collection name is too long".into()));
    }
    let length = i32::try_from(name_bytes.len())
        .map_err(|_| Error::Limit("collection name is too long".into()))?;
    let mut payload = Vec::with_capacity(12 + name_bytes.len());
    payload.extend_from_slice(&collection_id.to_be_bytes());
    payload.extend_from_slice(&length.to_be_bytes());
    payload.extend_from_slice(name_bytes);
    Ok(payload)
}

fn encode_index_definition(index: &SecondaryIndex, format_version: u16) -> Result<Vec<u8>> {
    if index.id.value() <= 0 || index.collection_id <= 0 {
        return Err(Error::Integrity(
            "index identifiers must be positive".into(),
        ));
    }
    let segment_count = i32::try_from(index.path.segments().len())
        .map_err(|_| Error::Limit("field path has too many segments".into()))?;
    if segment_count <= 0 || segment_count as usize > crate::MAX_FIELD_PATH_DEPTH {
        return Err(Error::Limit("field path depth is out of bounds".into()));
    }
    let mut payload = Vec::new();
    payload.extend_from_slice(&index.id.value().to_be_bytes());
    payload.extend_from_slice(&index.collection_id.to_be_bytes());
    payload.push(u8::from(index.unique));
    payload.extend_from_slice(&segment_count.to_be_bytes());
    for segment in index.path.segments() {
        let length = i32::try_from(segment.len())
            .map_err(|_| Error::Limit("field path segment is too long".into()))?;
        if segment.is_empty() || segment.len() > 1_024 || segment.contains('\0') {
            return Err(Error::InvalidInput("invalid field path segment".into()));
        }
        payload.extend_from_slice(&length.to_be_bytes());
        payload.extend_from_slice(segment.as_bytes());
    }
    if format_version >= PERSISTENT_INDEX_FORMAT_VERSION {
        if index.root_page_id == 0 || index.root_page_id > i64::MAX as u64 {
            return Err(Error::Integrity(
                "persistent index definition must name a valid root page".into(),
            ));
        }
        payload.extend_from_slice(&(index.root_page_id as i64).to_be_bytes());
        payload.extend_from_slice(&(index.root_generation as i64).to_be_bytes());
    }
    Ok(payload)
}

fn encode_document_mutation(
    collection_id: i64,
    document_id: DocumentId,
    document: &Document,
) -> Result<Vec<u8>> {
    let encoded_document = codec::encode_document(document)?;
    let document_length = i32::try_from(encoded_document.len())
        .map_err(|_| Error::Limit("document is too large".into()))?;
    let mut payload = Vec::with_capacity(20 + encoded_document.len());
    payload.extend_from_slice(&collection_id.to_be_bytes());
    payload.extend_from_slice(&document_id.value().to_be_bytes());
    payload.extend_from_slice(&document_length.to_be_bytes());
    payload.extend_from_slice(&encoded_document);
    Ok(payload)
}

fn decode_document_mutation(input: &mut Cursor<'_>) -> Result<(i64, i64, Document)> {
    let collection_id = input.i64("document collection identifier")?;
    let document_id = input.i64("document identifier")?;
    if collection_id <= 0 || document_id <= 0 {
        return Err(Error::Integrity(
            "document identifiers must be positive".into(),
        ));
    }
    let document_length = input.i32("document length")?;
    if document_length < 0 || document_length as usize > crate::MAX_DOCUMENT_BYTES {
        return Err(Error::Integrity("document length is out of bounds".into()));
    }
    let document_bytes = input.bytes_exact(document_length as usize, "document payload")?;
    input.finish("document record")?;
    let document = codec::decode_document(&document_bytes)?;
    Ok((collection_id, document_id, document))
}

pub(crate) fn validate_collection_name(name: &str) -> Result<()> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 63 || !bytes[0].is_ascii_alphabetic() && bytes[0] != b'_' {
        return Err(Error::InvalidInput(
            "collection name must start with an ASCII letter or underscore and contain at most 63 characters".into(),
        ));
    }
    if !bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'-')
    {
        return Err(Error::InvalidInput(
            "collection names accept only ASCII letters, digits, '_' and '-'".into(),
        ));
    }
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(Error::InvalidInput("database path cannot be empty".into()));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if absolute.exists() {
        return Ok(fs::canonicalize(absolute)?);
    }
    let parent = absolute
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let file_name = absolute
        .file_name()
        .ok_or_else(|| Error::InvalidInput("database path needs a file name".into()))?;
    Ok(fs::canonicalize(parent)?.join(file_name))
}

fn temporary_sibling_path(target: &Path) -> Result<PathBuf> {
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let file_name = target
        .file_name()
        .ok_or_else(|| Error::InvalidInput("compacted-copy path needs a file name".into()))?;
    let mut temporary_name = file_name.to_os_string();
    let counter = NEXT_COMPACTION_TEMP_ID.fetch_add(1, AtomicOrdering::Relaxed);
    temporary_name.push(format!(".swdb-tmp-{}-{counter}", std::process::id()));
    Ok(parent.join(temporary_name))
}

fn sidecar_path(path: &Path, suffix: &str) -> Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| Error::InvalidInput("database path needs a file name".into()))?;
    let mut lock_name = file_name.to_os_string();
    lock_name.push(suffix);
    Ok(parent.join(lock_name))
}

fn acquire_writer_lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(sidecar_path(path, ".writer.lock")?)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(Error::Locked),
        Err(TryLockError::Error(error)) => Err(Error::Io(error)),
    }
}

fn random_database_id() -> Result<[u8; 16]> {
    let mut id = [0_u8; 16];
    getrandom::fill(&mut id).map_err(Error::Randomness)?;
    id[6] = (id[6] & 0x0f) | 0x40;
    id[8] = (id[8] & 0x3f) | 0x80;
    Ok(id)
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> Result<()> {
    // Rust's portable File API has no directory-sync operation on Windows.
    Ok(())
}

#[cfg(feature = "crash-test")]
fn crash_before_write() {
    crash_pause_if("before_write");
}

#[cfg(not(feature = "crash-test"))]
fn crash_before_write() {}

#[cfg(feature = "crash-test")]
fn crash_during_write() -> bool {
    std::env::var("SEAWORLD_CRASH_AT").is_ok_and(|point| point == "during_write")
}

#[cfg(not(feature = "crash-test"))]
fn crash_during_write() -> bool {
    false
}

#[cfg(feature = "crash-test")]
fn crash_after_write() {
    crash_pause_if("after_write_before_sync");
}

#[cfg(not(feature = "crash-test"))]
fn crash_after_write() {}

#[cfg(feature = "crash-test")]
fn crash_after_sync() {
    crash_pause_if("after_sync_before_return");
}

#[cfg(not(feature = "crash-test"))]
fn crash_after_sync() {}

#[cfg(feature = "crash-test")]
fn crash_transaction_at(point: &str) {
    crash_pause_if(point);
}

#[cfg(not(feature = "crash-test"))]
fn crash_transaction_at(_point: &str) {}

#[cfg(feature = "crash-test")]
fn crash_transaction_during(point: &str) -> bool {
    std::env::var("SEAWORLD_CRASH_AT").is_ok_and(|configured| configured == point)
}

#[cfg(not(feature = "crash-test"))]
fn crash_transaction_during(_point: &str) -> bool {
    false
}

#[cfg(feature = "crash-test")]
fn crash_pause_if(point: &str) {
    if std::env::var("SEAWORLD_CRASH_AT").is_ok_and(|configured| configured == point) {
        crash_pause(point);
    }
}

#[cfg(not(feature = "crash-test"))]
fn crash_pause_if(_point: &str) {}

#[cfg(feature = "crash-test")]
fn crash_pause(point: &str) -> ! {
    if let Some(marker) = std::env::var_os("SEAWORLD_CRASH_READY_FILE")
        && let Err(error) = std::fs::write(marker, point.as_bytes())
    {
        eprintln!("could not write crash-test marker: {error}");
        std::process::exit(87);
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

#[cfg(not(feature = "crash-test"))]
fn crash_pause(_point: &str) -> ! {
    unreachable!("crash points are disabled without the crash-test feature")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_validation_matches_ascii_name_grammar() {
        assert!(validate_collection_name("users").is_ok());
        assert!(validate_collection_name("_private-2").is_ok());
        assert!(validate_collection_name("../users").is_err());
        assert!(validate_collection_name("üser").is_err());
        assert!(validate_collection_name(&"a".repeat(64)).is_err());
    }

    #[test]
    fn record_crc_covers_prefix_and_payload() {
        let record = encode_record(RecordType::CollectionCreate, 1, b"test").unwrap();
        let stored = u32::from_be_bytes(record[record.len() - 4..].try_into().unwrap());
        assert_eq!(stored, format::crc32c(&record[..record.len() - 4]));
    }
}
