// SPDX-License-Identifier: AGPL-3.0-only
//! Stable, bounded-page logical exports over an MVCC snapshot.
//!
//! The export covers visible logical collection/document data at one committed
//! version. Storage indexes and WAL tombstones are engine internals and are not
//! exported; deletion before the snapshot is represented by absence, while
//! writes/deletes after it do not change the pinned view.

use sha2::{Digest, Sha256};

use crate::{Database, Document, DocumentId, Error, Result, Snapshot, codec};

const PAGE_SIZE: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotDescriptor {
    pub source_id: String,
    pub snapshot_id: String,
    pub format_version: u16,
    pub write_epoch: i64,
    pub inventory_scope: &'static str,
    pub inventory_complete: bool,
    pub tombstone_policy: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCollection {
    pub name: String,
    pub document_count: u64,
    pub serialized_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotRecord {
    pub collection: String,
    pub key: DocumentId,
    pub document: Document,
    pub serialized_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotInventory {
    pub descriptor: SnapshotDescriptor,
    pub collections: Vec<SnapshotCollection>,
    pub record_count: u64,
    pub serialized_bytes: u64,
    pub sha256: String,
}

/// An MVCC-pinned logical export. Pages and inventory always refer to its one epoch.
pub struct SnapshotExport {
    snapshot: Snapshot,
    descriptor: SnapshotDescriptor,
}

impl Database {
    /// Begins a stable logical export. V1/V2 files are rejected because they do not
    /// provide the MVCC snapshot guarantee required by this contract.
    pub fn begin_snapshot_export(&self) -> Result<SnapshotExport> {
        let snapshot = self.begin_read()?;
        let version = snapshot.version().value();
        let source_id = hex(&self.database_id());
        let descriptor = SnapshotDescriptor {
            snapshot_id: format!("sw:{}:{version}", source_id),
            source_id,
            format_version: self.format_version(),
            write_epoch: version,
            inventory_scope: "all_logical_collections",
            inventory_complete: true,
            tombstone_policy: "pre_snapshot_deletes_absent;post_snapshot_changes_pinned",
        };
        Ok(SnapshotExport {
            snapshot,
            descriptor,
        })
    }
}

impl SnapshotExport {
    pub fn descriptor(&self) -> &SnapshotDescriptor {
        &self.descriptor
    }

    /// Lists all collection names visible at the snapshot epoch, including empty ones.
    pub fn list_collections(&self) -> Result<Vec<String>> {
        self.snapshot.collection_names()
    }

    /// Lists a stable, ascending page of records after an exclusive numeric cursor.
    pub fn list_documents(
        &self,
        collection: &str,
        after: Option<DocumentId>,
        limit: usize,
    ) -> Result<Vec<SnapshotRecord>> {
        let rows = self
            .snapshot
            .list_documents_after(collection, after, limit)?;
        rows.into_iter()
            .map(|row| {
                let encoded = codec::encode_document(&row.document)?;
                let digest = record_digest(collection, row.id, &encoded);
                Ok(SnapshotRecord {
                    collection: collection.to_owned(),
                    key: row.id,
                    document: row.document,
                    serialized_bytes: u64::try_from(encoded.len())
                        .map_err(|_| Error::Limit("snapshot record size overflow".into()))?,
                    sha256: hex(&digest),
                })
            })
            .collect()
    }

    /// Recomputes a complete deterministic inventory without retaining document
    /// payloads. The snapshot itself remains pinned until this export is dropped.
    pub fn inventory(&self) -> Result<SnapshotInventory> {
        let mut inventory_hash = Sha256::new();
        inventory_hash.update(b"SEAWORLD-LOGICAL-INVENTORY-V1\0");
        hash_bytes(&mut inventory_hash, self.descriptor.source_id.as_bytes());
        hash_bytes(&mut inventory_hash, self.descriptor.snapshot_id.as_bytes());
        inventory_hash.update(self.descriptor.format_version.to_be_bytes());
        inventory_hash.update(self.descriptor.write_epoch.to_be_bytes());

        let mut collections = Vec::new();
        let mut record_count = 0_u64;
        let mut total_bytes = 0_u64;
        for name in self.list_collections()? {
            let mut collection_hash = Sha256::new();
            collection_hash.update(b"SEAWORLD-COLLECTION-INVENTORY-V1\0");
            hash_bytes(&mut collection_hash, name.as_bytes());
            let mut collection_count = 0_u64;
            let mut collection_bytes = 0_u64;
            let mut cursor = None;
            loop {
                let rows = self.list_documents(&name, cursor, PAGE_SIZE)?;
                if rows.is_empty() {
                    break;
                }
                for row in &rows {
                    collection_hash.update(row.sha256.as_bytes());
                    collection_count = collection_count
                        .checked_add(1)
                        .ok_or_else(|| Error::Limit("snapshot record count overflow".into()))?;
                    collection_bytes = collection_bytes
                        .checked_add(row.serialized_bytes)
                        .ok_or_else(|| Error::Limit("snapshot byte count overflow".into()))?;
                    cursor = Some(row.key);
                }
                if rows.len() < PAGE_SIZE {
                    break;
                }
            }
            let collection_digest = collection_hash.finalize();
            hash_bytes(&mut inventory_hash, name.as_bytes());
            inventory_hash.update(collection_count.to_be_bytes());
            inventory_hash.update(collection_bytes.to_be_bytes());
            inventory_hash.update(collection_digest);
            record_count = record_count
                .checked_add(collection_count)
                .ok_or_else(|| Error::Limit("snapshot record count overflow".into()))?;
            total_bytes = total_bytes
                .checked_add(collection_bytes)
                .ok_or_else(|| Error::Limit("snapshot byte count overflow".into()))?;
            collections.push(SnapshotCollection {
                name,
                document_count: collection_count,
                serialized_bytes: collection_bytes,
                sha256: hex(&collection_digest),
            });
        }
        Ok(SnapshotInventory {
            descriptor: self.descriptor.clone(),
            collections,
            record_count,
            serialized_bytes: total_bytes,
            sha256: hex(&inventory_hash.finalize()),
        })
    }

    /// Reports whether the live handle is still at the snapshot's source identity
    /// and epoch. A pinned snapshot remains readable even when this returns false.
    pub fn is_current(&self, database: &Database) -> Result<bool> {
        Ok(hex(&database.database_id()) == self.descriptor.source_id
            && database.format_version() == self.descriptor.format_version
            && database.current_version()?.value() == self.descriptor.write_epoch)
    }
}

fn record_digest(collection: &str, key: DocumentId, encoded_document: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"SEAWORLD-LOGICAL-RECORD-V1\0");
    hash_bytes(&mut hash, collection.as_bytes());
    hash.update(key.value().to_be_bytes());
    hash_bytes(&mut hash, encoded_document);
    hash.finalize().into()
}

fn hash_bytes(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_be_bytes());
    hash.update(value);
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}
