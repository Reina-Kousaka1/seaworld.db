use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use seaworld_db::{Database, Document, DocumentId, Error, Value};

const JAVA_V1_FIXTURE: &[u8] = include_bytes!("fixtures/java-v1-basic.fixture");
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

struct TempDatabase {
    path: PathBuf,
}

impl TempDatabase {
    fn new() -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "seaworld-db-{}-{id}.seaworld.db",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        Self { path }
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn sample_document() -> Document {
    Document::new(BTreeMap::from([
        ("credits".into(), Value::Integer(1250)),
        (
            "inventory".into(),
            Value::Array(vec![
                Value::String("pink_leotard".into()),
                Value::String("ballet_shoes".into()),
            ]),
        ),
        (
            "profile".into(),
            Value::object([
                ("career".into(), Value::String("ballet".into())),
                ("level".into(), Value::Integer(17)),
            ]),
        ),
        ("username".into(), Value::String("Charlotte".into())),
    ]))
}

fn append_record(path: &PathBuf, kind: u8, sequence: i64, payload: &[u8]) {
    let block_length = 24 + payload.len();
    let mut block = Vec::with_capacity(block_length);
    block.extend_from_slice(b"SWBL");
    block.push(kind);
    block.push(0);
    block.extend_from_slice(&0_u16.to_be_bytes());
    block.extend_from_slice(&(block_length as i32).to_be_bytes());
    block.extend_from_slice(&sequence.to_be_bytes());
    block.extend_from_slice(payload);
    let checksum = crc32c(&block);
    block.extend_from_slice(&checksum.to_be_bytes());
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(&block)
        .unwrap();
}

fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0x82f6_3b78 & mask);
        }
    }
    !crc
}

fn raw_empty_document_insert(collection_id: i64, document_id: i64) -> Vec<u8> {
    let document = 0_i32.to_be_bytes();
    let mut payload = Vec::with_capacity(20 + document.len());
    payload.extend_from_slice(&collection_id.to_be_bytes());
    payload.extend_from_slice(&document_id.to_be_bytes());
    payload.extend_from_slice(&(document.len() as i32).to_be_bytes());
    payload.extend_from_slice(&document);
    payload
}

#[test]
fn create_database_writes_a_valid_64_byte_header() {
    let temp = TempDatabase::new();
    let database = Database::create(&temp.path).unwrap();
    let database_id = database.database_id();
    database.close().unwrap();
    assert_eq!(fs::metadata(&temp.path).unwrap().len(), 64);
    assert_eq!(&fs::read(&temp.path).unwrap()[..8], b"SWDB\r\n\x1a\n");
    assert_ne!(database_id, [0; 16]);
}

#[test]
fn empty_database_reopens() {
    let temp = TempDatabase::new();
    Database::create(&temp.path).unwrap().close().unwrap();
    let database = Database::open(&temp.path).unwrap();
    assert!(database.collection_names().unwrap().is_empty());
    assert_eq!(database.recovery_report().discarded_bytes(), 0);
}

#[test]
fn collection_persists_after_reopen() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.path).unwrap();
    assert_eq!(database.create_collection("users").unwrap().value(), 1);
    database.close().unwrap();
    let database = Database::open(&temp.path).unwrap();
    assert_eq!(database.collection_names().unwrap(), ["users"]);
}

#[test]
fn insert_and_read_document() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.path).unwrap();
    database.create_collection("users").unwrap();
    let id = database.insert("users", sample_document()).unwrap();
    assert_eq!(id.value(), 1);
    assert_eq!(database.get("users", id).unwrap(), Some(&sample_document()));
}

#[test]
fn update_persists_after_reopen() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.path).unwrap();
    database.create_collection("users").unwrap();
    let id = database.insert("users", sample_document()).unwrap();
    let updated = sample_document().with("credits", Value::Integer(2000));
    assert!(database.update("users", id, updated.clone()).unwrap());
    database.close().unwrap();
    let database = Database::open(&temp.path).unwrap();
    assert_eq!(database.get("users", id).unwrap(), Some(&updated));
}

#[test]
fn delete_persists_after_reopen() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.path).unwrap();
    database.create_collection("users").unwrap();
    let id = database.insert("users", sample_document()).unwrap();
    assert!(database.delete("users", id).unwrap());
    database.close().unwrap();
    let database = Database::open(&temp.path).unwrap();
    assert_eq!(database.get("users", id).unwrap(), None);
    assert!(database.document_ids("users").unwrap().is_empty());
}

#[test]
fn nested_documents_arrays_and_binary_values_round_trip() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.path).unwrap();
    database.create_collection("users").unwrap();
    let document = Document::new(BTreeMap::from([
        ("binary".into(), Value::Binary(vec![0, 1, 127, 255])),
        ("enabled".into(), Value::Bool(true)),
        ("nothing".into(), Value::Null),
        ("ratio".into(), Value::Float(1.25)),
        (
            "nested".into(),
            sample_document().get("profile").unwrap().clone(),
        ),
    ]));
    let id = database.insert("users", document.clone()).unwrap();
    database.close().unwrap();
    let database = Database::open(&temp.path).unwrap();
    assert_eq!(database.get("users", id).unwrap(), Some(&document));
}

#[test]
fn multiple_collections_stay_separate() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.path).unwrap();
    database.create_collection("users").unwrap();
    database.create_collection("inventory").unwrap();
    let user_id = database.insert("users", sample_document()).unwrap();
    let item_id = database.insert("inventory", Document::empty()).unwrap();
    assert!(database.get("users", item_id).unwrap().is_none());
    assert!(database.get("inventory", user_id).unwrap().is_none());
    assert_eq!(database.collection_names().unwrap(), ["inventory", "users"]);
}

#[test]
fn data_survives_a_full_close_and_reopen() {
    let temp = TempDatabase::new();
    let expected = sample_document();
    let id = {
        let mut database = Database::create(&temp.path).unwrap();
        database.create_collection("users").unwrap();
        let id = database.insert("users", expected.clone()).unwrap();
        database.close().unwrap();
        id
    };
    let database = Database::open(&temp.path).unwrap();
    assert_eq!(database.get("users", id).unwrap(), Some(&expected));
}

#[test]
fn java_v1_fixture_opens_in_rust() {
    let temp = TempDatabase::new();
    fs::write(&temp.path, JAVA_V1_FIXTURE).unwrap();
    let database = Database::open(&temp.path).unwrap();
    assert_eq!(database.collection_names().unwrap(), ["users"]);
    assert_eq!(
        database.get("users", DocumentId::new(42).unwrap()).unwrap(),
        Some(&sample_document())
    );
}

#[test]
fn rust_records_are_byte_compatible_with_the_java_v1_fixture() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.path).unwrap();
    database.create_collection("users").unwrap();
    database
        .insert_with_id("users", DocumentId::new(42).unwrap(), sample_document())
        .unwrap();
    database.close().unwrap();
    let rust_file = fs::read(&temp.path).unwrap();
    assert_eq!(&rust_file[64..], &JAVA_V1_FIXTURE[64..]);
}

#[test]
fn invalid_magic_is_rejected() {
    let temp = TempDatabase::new();
    Database::create(&temp.path).unwrap().close().unwrap();
    let mut bytes = fs::read(&temp.path).unwrap();
    bytes[0] ^= 0x20;
    fs::write(&temp.path, bytes).unwrap();
    assert!(matches!(Database::open(&temp.path), Err(Error::Format(_))));
}

#[test]
fn truncated_header_is_rejected() {
    let temp = TempDatabase::new();
    File::create(&temp.path)
        .unwrap()
        .write_all(b"SWDB\r\n")
        .unwrap();
    assert!(matches!(Database::open(&temp.path), Err(Error::Format(_))));
}

#[test]
fn incomplete_tail_is_reported_recovered_and_idempotent() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.path).unwrap();
    database.create_collection("users").unwrap();
    let id = database.insert("users", sample_document()).unwrap();
    database.close().unwrap();
    OpenOptions::new()
        .append(true)
        .open(&temp.path)
        .unwrap()
        .write_all(b"SWB")
        .unwrap();

    let database = Database::open(&temp.path).unwrap();
    assert_eq!(database.recovery_report().discarded_bytes(), 3);
    assert_eq!(database.get("users", id).unwrap(), Some(&sample_document()));
    database.close().unwrap();
    let database = Database::open(&temp.path).unwrap();
    assert_eq!(database.recovery_report().discarded_bytes(), 0);
}

#[test]
fn incomplete_declared_block_body_is_discarded() {
    let temp = TempDatabase::new();
    Database::create(&temp.path).unwrap().close().unwrap();
    let mut prefix = Vec::from(&b"SWBL\x01\0\0\0"[..]);
    prefix.extend_from_slice(&100_i32.to_be_bytes());
    prefix.extend_from_slice(&1_i64.to_be_bytes());
    OpenOptions::new()
        .append(true)
        .open(&temp.path)
        .unwrap()
        .write_all(&prefix)
        .unwrap();
    let database = Database::open(&temp.path).unwrap();
    assert_eq!(database.recovery_report().discarded_bytes(), 20);
    assert!(database.collection_names().unwrap().is_empty());
}

#[test]
fn duplicate_document_ids_are_rejected_by_the_api() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.path).unwrap();
    database.create_collection("users").unwrap();
    database.create_collection("archive").unwrap();
    let id = DocumentId::new(42).unwrap();
    database
        .insert_with_id("users", id, Document::empty())
        .unwrap();
    assert!(matches!(
        database.insert_with_id("archive", id, Document::empty()),
        Err(Error::Duplicate(_))
    ));
}

#[test]
fn duplicate_document_ids_in_a_file_are_rejected() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.path).unwrap();
    database.create_collection("users").unwrap();
    database.create_collection("archive").unwrap();
    database
        .insert_with_id("users", DocumentId::new(42).unwrap(), Document::empty())
        .unwrap();
    database.close().unwrap();
    append_record(&temp.path, 2, 4, &raw_empty_document_insert(2, 42));
    assert!(matches!(
        Database::open(&temp.path),
        Err(Error::Integrity(_))
    ));
}

#[test]
fn primary_id_index_survives_replay_across_collections() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.path).unwrap();
    database.create_collection("left").unwrap();
    database.create_collection("right").unwrap();
    for value in 1..=200_i64 {
        let name = if value % 2 == 0 { "left" } else { "right" };
        database
            .insert_with_id(name, DocumentId::new(value).unwrap(), Document::empty())
            .unwrap();
    }
    database.close().unwrap();
    let database = Database::open(&temp.path).unwrap();
    for value in 1..=200_i64 {
        let owner = if value % 2 == 0 { "left" } else { "right" };
        let other = if value % 2 == 0 { "right" } else { "left" };
        let id = DocumentId::new(value).unwrap();
        assert!(database.get(owner, id).unwrap().is_some());
        assert!(database.get(other, id).unwrap().is_none());
    }
}

#[test]
fn complete_record_checksum_mismatch_is_rejected() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.path).unwrap();
    database.create_collection("users").unwrap();
    database.close().unwrap();
    let mut bytes = fs::read(&temp.path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(&temp.path, bytes).unwrap();
    assert!(matches!(
        Database::open(&temp.path),
        Err(Error::Integrity(_))
    ));
}

#[test]
fn invalid_utf8_in_document_field_is_rejected() {
    let temp = TempDatabase::new();
    Database::create(&temp.path).unwrap().close().unwrap();
    let mut collection_payload = 1_i64.to_be_bytes().to_vec();
    collection_payload.extend_from_slice(&5_i32.to_be_bytes());
    collection_payload.extend_from_slice(b"users");
    append_record(&temp.path, 1, 1, &collection_payload);

    let mut invalid_document = 1_i32.to_be_bytes().to_vec();
    invalid_document.extend_from_slice(&1_i32.to_be_bytes());
    invalid_document.push(0xff);
    invalid_document.push(0);
    let mut insert_payload = 1_i64.to_be_bytes().to_vec();
    insert_payload.extend_from_slice(&1_i64.to_be_bytes());
    insert_payload.extend_from_slice(&(invalid_document.len() as i32).to_be_bytes());
    insert_payload.extend_from_slice(&invalid_document);
    append_record(&temp.path, 2, 2, &insert_payload);
    assert!(matches!(
        Database::open(&temp.path),
        Err(Error::Integrity(_))
    ));
}

#[test]
fn oversized_record_length_is_rejected_before_allocation() {
    let temp = TempDatabase::new();
    Database::create(&temp.path).unwrap().close().unwrap();
    let mut prefix = b"SWBL\x01\0\0\0".to_vec();
    prefix.extend_from_slice(&(17 * 1024 * 1024 + 1_i32).to_be_bytes());
    prefix.extend_from_slice(&1_i64.to_be_bytes());
    OpenOptions::new()
        .append(true)
        .open(&temp.path)
        .unwrap()
        .write_all(&prefix)
        .unwrap();
    assert!(matches!(Database::open(&temp.path), Err(Error::Format(_))));
}

#[test]
fn collection_names_cannot_contain_path_traversal() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.path).unwrap();
    assert!(matches!(
        database.create_collection("../users"),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        database.create_collection("user/name"),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn excessively_nested_documents_are_rejected() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.path).unwrap();
    database.create_collection("users").unwrap();
    let mut nested = Value::Null;
    for _ in 0..65 {
        nested = Value::Array(vec![nested]);
    }
    let document = Document::new(BTreeMap::from([("deep".into(), nested)]));
    assert!(matches!(
        database.insert("users", document),
        Err(Error::Limit(_))
    ));
}

#[test]
fn create_never_overwrites_an_existing_file() {
    let temp = TempDatabase::new();
    File::create(&temp.path)
        .unwrap()
        .write_all(b"keep this")
        .unwrap();
    assert!(matches!(Database::create(&temp.path), Err(Error::Io(_))));
    assert_eq!(fs::read(&temp.path).unwrap(), b"keep this");
}

#[test]
fn one_process_cannot_open_a_second_writer() {
    let temp = TempDatabase::new();
    let database = Database::create(&temp.path).unwrap();
    assert!(matches!(Database::open(&temp.path), Err(Error::Locked)));
    database.close().unwrap();
}

#[test]
fn non_finite_float_values_are_rejected() {
    assert!(Value::floating(f64::NAN).is_err());
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.path).unwrap();
    database.create_collection("users").unwrap();
    let invalid = Document::new(BTreeMap::from([(
        "bad".into(),
        Value::Float(f64::INFINITY),
    )]));
    assert!(matches!(
        database.insert("users", invalid),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn negative_document_ids_are_rejected() {
    assert!(matches!(DocumentId::new(0), Err(Error::InvalidInput(_))));
    assert!(matches!(DocumentId::new(-1), Err(Error::InvalidInput(_))));
}

#[test]
fn compacted_copy_preserves_live_data_and_does_not_replace_the_source() {
    let source = TempDatabase::new();
    let target = TempDatabase::new();
    let mut database = Database::create(&source.path).unwrap();
    database.create_collection("users").unwrap();
    let id = DocumentId::new(42).unwrap();
    database
        .insert_with_id("users", id, sample_document())
        .unwrap();
    for credits in [2_000, 3_000, 4_000] {
        let updated = sample_document().with("credits", Value::Integer(credits));
        assert!(database.update("users", id, updated).unwrap());
    }
    let deleted_id = database.insert("users", Document::empty()).unwrap();
    assert!(database.delete("users", deleted_id).unwrap());
    database.close().unwrap();
    let source_bytes = fs::read(&source.path).unwrap();
    let database = Database::open(&source.path).unwrap();
    let database_id = database.database_id();

    database.write_compacted_copy(&target.path).unwrap();

    database.close().unwrap();
    assert_eq!(fs::read(&source.path).unwrap(), source_bytes);
    assert!(fs::metadata(&target.path).unwrap().len() < source_bytes.len() as u64);
    let compacted = Database::open(&target.path).unwrap();
    assert_eq!(compacted.database_id(), database_id);
    assert_eq!(compacted.document_ids("users").unwrap(), [id]);
    assert_eq!(
        compacted.get("users", id).unwrap(),
        Some(&sample_document().with("credits", Value::Integer(4_000)))
    );
}

#[test]
fn compacted_copy_never_overwrites_its_destination_or_source() {
    let source = TempDatabase::new();
    let target = TempDatabase::new();
    let mut database = Database::create(&source.path).unwrap();
    database.create_collection("users").unwrap();
    File::create(&target.path)
        .unwrap()
        .write_all(b"preserve destination")
        .unwrap();
    assert!(matches!(
        database.write_compacted_copy(&target.path),
        Err(Error::Duplicate(_))
    ));
    assert_eq!(fs::read(&target.path).unwrap(), b"preserve destination");
    assert!(matches!(
        database.write_compacted_copy(&source.path),
        Err(Error::InvalidInput(_))
    ));
    database.close().unwrap();
    assert_eq!(&fs::read(&source.path).unwrap()[..8], b"SWDB\r\n\x1a\n");
}
