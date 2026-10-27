use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions, TryLockError},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    CollectionId, Document, DocumentId, Error, Result,
    codec::{self, Cursor},
    format::{
        self, HEADER_SIZE, MAX_RECORD_SIZE, MIN_RECORD_SIZE, RECORD_MAGIC, RECORD_PREFIX_SIZE,
    },
};

/// Maximum supported file size for version 1.
pub const MAX_DATABASE_BYTES: u64 = format::MAX_DATABASE_SIZE_BYTES;
/// Maximum active collections in one database.
pub const MAX_COLLECTIONS: usize = 10_000;
/// Maximum active documents in one database.
pub const MAX_DOCUMENTS: usize = 100_000;

const COLLECTION_NAME_MAX_BYTES: usize = 255;
static NEXT_COMPACTION_TEMP_ID: AtomicU64 = AtomicU64::new(1);

/// Number of bytes discarded from an incomplete final record while opening.
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

struct CollectionState {
    id: i64,
    documents: BTreeMap<i64, Document>,
}

/// Embedded database handle backed by SeaWorld's version 1 append-only file.
///
/// A handle owns an exclusive OS file lock. A write is durable only after its
/// complete checksummed record has been synchronized successfully.
pub struct Database {
    path: PathBuf,
    file: File,
    database_id: [u8; 16],
    collections: HashMap<String, CollectionState>,
    collection_names_by_id: HashMap<i64, String>,
    primary_id_index: HashMap<i64, i64>,
    next_sequence: i64,
    next_collection_id: i64,
    next_document_id: i64,
    document_ids_exhausted: bool,
    document_count: usize,
    write_failed: bool,
    recovery_report: RecoveryReport,
}

impl Database {
    /// Creates a new database file and never overwrites an existing path.
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let path = absolute_path(path.as_ref())?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        acquire_lock(&file)?;

        let database_id = random_database_id()?;
        let header = format::encode_header(database_id);
        file.write_all(&header)?;
        file.sync_all()?;
        sync_parent_directory(&path)?;

        Ok(Self::new(path, file, database_id))
    }

    /// Opens a database, validates the file, and replays complete records.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = absolute_path(path.as_ref())?;
        let mut file = OpenOptions::new().read(true).write(true).open(&path)?;
        acquire_lock(&file)?;
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
        let database_id = format::decode_header(&header)?;
        let mut database = Self::new(path, file, database_id);
        database.replay_records(file_size)?;
        Ok(database)
    }

    fn new(path: PathBuf, file: File, database_id: [u8; 16]) -> Self {
        Self {
            path,
            file,
            database_id,
            collections: HashMap::new(),
            collection_names_by_id: HashMap::new(),
            primary_id_index: HashMap::new(),
            next_sequence: 1,
            next_collection_id: 1,
            next_document_id: 1,
            document_ids_exhausted: false,
            document_count: 0,
            write_failed: false,
            recovery_report: RecoveryReport::default(),
        }
    }

    /// Returns the normalized path used to open the database.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the UUID bytes stored in the database header.
    pub const fn database_id(&self) -> [u8; 16] {
        self.database_id
    }

    /// Reports any incomplete final record discarded during open.
    pub const fn recovery_report(&self) -> RecoveryReport {
        self.recovery_report
    }

    /// Persists a new collection and returns its database-local identifier.
    pub fn create_collection(&mut self, name: &str) -> Result<CollectionId> {
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

    /// Writes a compacted, byte-compatible copy without replacing this database.
    ///
    /// The new file preserves collection/document IDs and the database UUID, but
    /// rewrites only currently active documents. It is published without
    /// overwriting an existing destination. The destination filesystem must
    /// support hard links; otherwise this returns an I/O error and leaves this
    /// database unchanged.
    pub fn write_compacted_copy(&self, target: impl AsRef<Path>) -> Result<()> {
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

    /// Synchronizes the database and releases its exclusive file lock.
    pub fn close(self) -> Result<()> {
        let sync_result = self.file.sync_all();
        let unlock_result = self.file.unlock();
        match (sync_result, unlock_result) {
            (Err(error), _) => Err(Error::Io(error)),
            (Ok(()), Err(error)) => Err(Error::Io(error)),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    fn replay_records(&mut self, file_size: u64) -> Result<()> {
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
        }
        Ok(())
    }

    fn recover_incomplete_tail(&mut self, last_good_offset: u64, original_size: u64) -> Result<()> {
        self.file.set_len(last_good_offset)?;
        self.file.sync_all()?;
        self.recovery_report = RecoveryReport {
            discarded_bytes: original_size - last_good_offset,
        };
        Ok(())
    }

    fn append_record(&mut self, record_type: RecordType, payload: &[u8]) -> Result<()> {
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
        self.file.seek(SeekFrom::End(0))?;
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
        if let Err(error) = write_result {
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecordType {
    CollectionCreate,
    DocumentInsert,
    DocumentUpdate,
    DocumentDelete,
}

impl RecordType {
    const fn code(self) -> u8 {
        match self {
            Self::CollectionCreate => 1,
            Self::DocumentInsert => 2,
            Self::DocumentUpdate => 3,
            Self::DocumentDelete => 4,
        }
    }

    fn from_code(code: u8) -> Result<Self> {
        match code {
            1 => Ok(Self::CollectionCreate),
            2 => Ok(Self::DocumentInsert),
            3 => Ok(Self::DocumentUpdate),
            4 => Ok(Self::DocumentDelete),
            _ => Err(Error::Format(format!("unknown record t