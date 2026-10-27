package org.seaworlddb;

import java.io.ByteArrayOutputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.channels.FileChannel;
import java.nio.channels.FileLock;
import java.nio.channels.OverlappingFileLockException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.TreeMap;
import java.util.TreeSet;
import java.util.UUID;

/**
 * Embedded SeaWorld database handle backed by its own append-only file format.
 * A handle holds an exclusive file lock until it is closed.
 */
public final class Database implements AutoCloseable {
    private static final int COLLECTION_NAME_MAX_BYTES = 255;
    private static final int MAX_ZERO_LENGTH_WRITES = 8;

    private final Path path;
    private final FileChannel channel;
    private final FileLock fileLock;
    private final UUID databaseId;
    private final Map<String, CollectionState> collections = new HashMap<>();
    private final Map<Long, CollectionState> collectionsById = new HashMap<>();
    /** Primary-ID index rebuilt from verified blocks during open. */
    private final Map<Long, CollectionState> primaryIdIndex = new HashMap<>();

    private long nextSequence = 1;
    private long nextCollectionId = 1;
    private long nextDocumentId = 1;
    private long documentCount;
    private boolean documentIdsExhausted;
    private boolean closed;
    private boolean writeFailed;
    private RecoveryReport recoveryReport = new RecoveryReport(0);

    private Database(Path path, FileChannel channel, FileLock fileLock, UUID databaseId) {
        this.path = path;
        this.channel = channel;
        this.fileLock = fileLock;
        this.databaseId = databaseId;
    }

    /** Creates a new database file and never overwrites an existing path. */
    public static Database create(Path path) throws IOException {
        Path absolutePath = normalizePath(path);
        FileChannel channel = FileChannel.open(absolutePath,
                StandardOpenOption.CREATE_NEW,
                StandardOpenOption.READ,
                StandardOpenOption.WRITE);
        FileLock lock = null;
        try {
            lock = acquireLock(channel, absolutePath);
            UUID databaseId = UUID.randomUUID();
            writeFully(channel, ByteBuffer.wrap(DatabaseFormat.encodeHeader(databaseId)), 0);
            channel.force(true);
            return new Database(absolutePath, channel, lock, databaseId);
        } catch (IOException | RuntimeException exception) {
            if (lock != null) {
                try {
                    lock.release();
                } catch (IOException releaseFailure) {
                    exception.addSuppressed(releaseFailure);
                }
            }
            try {
                channel.close();
            } catch (IOException closeFailure) {
                exception.addSuppressed(closeFailure);
            }
            throw exception;
        }
    }

    /** Opens and validates a database, replaying all complete operation blocks. */
    public static Database open(Path path) throws IOException {
        Path absolutePath = normalizePath(path);
        FileChannel channel = FileChannel.open(absolutePath,
                StandardOpenOption.READ,
                StandardOpenOption.WRITE);
        FileLock lock = null;
        try {
            lock = acquireLock(channel, absolutePath);
            long fileSize = channel.size();
            if (fileSize > DatabaseFormat.MAX_DATABASE_SIZE_BYTES) {
                throw new DatabaseFormatException("Database file exceeds the Phase 0 size limit");
            }
            if (fileSize < DatabaseFormat.HEADER_SIZE) {
                throw new DatabaseFormatException("Database file is shorter than its header");
            }
            byte[] headerBytes = new byte[DatabaseFormat.HEADER_SIZE];
            readFully(channel, ByteBuffer.wrap(headerBytes), 0);
            DatabaseFormat.Header header = DatabaseFormat.decodeHeader(headerBytes);
            Database database = new Database(absolutePath, channel, lock, header.databaseId());
            database.replayRecords();
            return database;
        } catch (IOException | RuntimeException exception) {
            if (lock != null) {
                try {
                    lock.release();
                } catch (IOException releaseFailure) {
                    exception.addSuppressed(releaseFailure);
                }
            }
            try {
                channel.close();
            } catch (IOException closeFailure) {
                exception.addSuppressed(closeFailure);
            }
            throw exception;
        }
    }

    public Path path() {
        return path;
    }

    public UUID databaseId() {
        return databaseId;
    }

    /** Reports any incomplete final block discarded during open. */
    public RecoveryReport recoveryReport() {
        return recoveryReport;
    }

    public synchronized CollectionId createCollection(String name) throws IOException {
        checkUsable();
        validateCollectionName(name);
        if (collections.containsKey(name)) {
            throw new DatabaseException("Collection already exists: " + name);
        }
        if (collections.size() >= DatabaseFormat.MAX_COLLECTIONS_PER_DATABASE) {
            throw new DatabaseException("Database collection limit reached");
        }
        if (nextCollectionId <= 0) {
            throw new DatabaseException("Collection identifier space is exhausted");
        }
        long collectionId = nextCollectionId;
        byte[] payload = encodeCollectionCreate(collectionId, name);
        appendRecord(RecordType.COLLECTION_CREATE, payload);
        CollectionState state = new CollectionState(collectionId, name);
        collections.put(name, state);
        collectionsById.put(collectionId, state);
        nextCollectionId = collectionId == Long.MAX_VALUE ? 0 : collectionId + 1;
        return new CollectionId(collectionId);
    }

    /** Inserts a document and assigns the next positive database-local identifier. */
    public synchronized DocumentId insert(String collectionName, Document document)
            throws IOException {
        checkUsable();
        if (documentIdsExhausted || nextDocumentId <= 0) {
            throw new DatabaseException("Document identifier space is exhausted");
        }
        DocumentId id = new DocumentId(nextDocumentId);
        insert(collectionName, id, document);
        return id;
    }

    /** Inserts using a caller-supplied identifier, rejecting duplicates across the database. */
    public synchronized void insert(String collectionName, DocumentId id, Document document)
            throws IOException {
        checkUsable();
        CollectionState collection = requireCollection(collectionName);
        if (primaryIdIndex.containsKey(id.value())) {
            throw new DatabaseException("Duplicate document ID: " + id.value());
        }
        if (documentCount >= DatabaseFormat.MAX_DOCUMENTS_PER_DATABASE) {
            throw new DatabaseException("Database document limit reached");
        }
        byte[] payload = encodeDocumentMutation(collection.id, id, document);
        appendRecord(RecordType.DOCUMENT_INSERT, payload);
        collection.documents.put(id.value(), document);
        primaryIdIndex.put(id.value(), collection);
        documentCount++;
        advanceNextDocumentId(id.value());
    }

    /** Reads a document by primary ID from the named collection. */
    public synchronized Optional<Document> get(String collectionName, DocumentId id)
            throws IOException {
        checkUsable();
        CollectionState collection = requireCollection(collectionName);
        if (primaryIdIndex.get(id.value()) != collection) {
            return Optional.empty();
        }
        return Optional.ofNullable(collection.documents.get(id.value()));
    }

    /** Replaces a document as one logged operation. Returns false if the ID is absent. */
    public synchronized boolean update(String collectionName, DocumentId id, Document document)
            throws IOException {
        checkUsable();
        CollectionState collection = requireCollection(collectionName);
        if (primaryIdIndex.get(id.value()) != collection) {
            return false;
        }
        byte[] payload = encodeDocumentMutation(collection.id, id, document);
        appendRecord(RecordType.DOCUMENT_UPDATE, payload);
        collection.documents.put(id.value(), document);
        return true;
    }

    /** Deletes a document as one logged operation. Returns false if the ID is absent. */
    public synchronized boolean delete(String collectionName, DocumentId id) throws IOException {
        checkUsable();
        CollectionState collection = requireCollection(collectionName);
        if (primaryIdIndex.get(id.value()) != collection) {
            return false;
        }
        appendRecord(RecordType.DOCUMENT_DELETE, encodeDocumentDelete(collection.id, id));
        collection.documents.remove(id.value());
        primaryIdIndex.remove(id.value());
        documentCount--;
        return true;
    }

    /** Returns identifiers in ascending order. */
    public synchronized List<DocumentId> documentIds(String collectionName) throws IOException {
        checkUsable();
        CollectionState collection = requireCollection(collectionName);
        ArrayList<DocumentId> result = new ArrayList<>(collection.documents.size());
        for (long id : collection.documents.keySet()) {
            result.add(new DocumentId(id));
        }
        return List.copyOf(result);
    }

    public synchronized List<String> collectionNames() throws IOException {
        checkUsable();
        return List.copyOf(new TreeSet<>(collections.keySet()));
    }

    @Override
    public synchronized void close() throws IOException {
        if (closed) {
            return;
        }
        closed = true;
        IOException failure = null;
        try {
            fileLock.release();
        } catch (IOException exception) {
            failure = exception;
        }
        try {
            channel.close();
        } catch (IOException exception) {
            if (failure == null) {
                failure = exception;
            } else {
                failure.addSuppressed(exception);
            }
        }
        if (failure != null) {
            throw failure;
        }
    }

    private void replayRecords() throws IOException {
        long fileSize = channel.size();
        if (fileSize > DatabaseFormat.MAX_DATABASE_SIZE_BYTES) {
            throw new DatabaseFormatException("Database file exceeds the Phase 0 size limit");
        }
        long offset = DatabaseFormat.HEADER_SIZE;
        while (offset < fileSize) {
            long remaining = fileSize - offset;
            if (remaining < DatabaseFormat.RECORD_PREFIX_SIZE) {
                recoverIncompleteTail(offset, fileSize);
                return;
            }

            byte[] prefixBytes = new byte[DatabaseFormat.RECORD_PREFIX_SIZE];
            readFully(channel, ByteBuffer.wrap(prefixBytes), offset);
            ByteBuffer prefix = ByteBuffer.wrap(prefixBytes).order(ByteOrder.BIG_ENDIAN);
            byte[] magic = new byte[DatabaseFormat.RECORD_MAGIC.length];
            prefix.get(magic);
            if (!java.util.Arrays.equals(magic, DatabaseFormat.RECORD_MAGIC)) {
                throw new DatabaseIntegrityException("Invalid record magic at file offset " + offset);
            }
            int typeCode = Byte.toUnsignedInt(prefix.get());
            int flags = Byte.toUnsignedInt(prefix.get());
            int reserved = Short.toUnsignedInt(prefix.getShort());
            int blockLength = prefix.getInt();
            long sequence = prefix.getLong();
            if (flags != 0 || reserved != 0) {
                throw new DatabaseFormatException("Unsupported record flags at offset " + offset);
            }
            if (blockLength < DatabaseFormat.MIN_RECORD_SIZE
                    || blockLength > DatabaseFormat.MAX_RECORD_SIZE) {
                throw new DatabaseFormatException("Record length is out of bounds at offset " + offset);
            }
            if (blockLength > remaining) {
                recoverIncompleteTail(offset, fileSize);
                return;
            }

            byte[] block = new byte[blockLength];
            readFully(channel, ByteBuffer.wrap(block), offset);
            int storedChecksum = ByteBuffer.wrap(block, block.length - Integer.BYTES,
                    Integer.BYTES).order(ByteOrder.BIG_ENDIAN).getInt();
            int actualChecksum = DatabaseFormat.crc32c(block, 0,
                    block.length - Integer.BYTES);
            if (storedChecksum != actualChecksum) {
                throw new DatabaseIntegrityException("Record checksum mismatch at offset " + offset);
            }
            if (sequence != nextSequence) {
                throw new DatabaseIntegrityException("Unexpected record sequence at offset " + offset);
            }
            if (nextSequence == Long.MAX_VALUE) {
                throw new DatabaseIntegrityException("Record sequence space is exhausted");
            }
            RecordType type = RecordType.fromCode(typeCode);
            int payloadLength = block.length - DatabaseFormat.MIN_RECORD_SIZE;
            byte[] payload = java.util.Arrays.copyOfRange(block,
                    DatabaseFormat.RECORD_PREFIX_SIZE,
                    DatabaseFormat.RECORD_PREFIX_SIZE + payloadLength);
            replayRecord(type, payload);
            nextSequence++;
            offset += blockLength;
        }
    }

    private void replayRecord(RecordType type, byte[] payload) throws IOException {
        ByteBuffer input = ByteBuffer.wrap(payload).order(ByteOrder.BIG_ENDIAN);
        switch (type) {
            case COLLECTION_CREATE -> {
                requireRemaining(input, Long.BYTES, "collection identifier");
                long collectionId = input.getLong();
                if (collectionId <= 0 || collectionId != nextCollectionId) {
                    throw new DatabaseIntegrityException("Invalid collection identifier in record");
                }
                String name = DocumentCodec.readUtf8(input, COLLECTION_NAME_MAX_BYTES,
                        "collection name");
                try {
                    validateCollectionName(name);
                } catch (IllegalArgumentException exception) {
                    throw new DatabaseIntegrityException("Invalid collection name in record");
                }
                if (input.hasRemaining() || collections.containsKey(name)
                        || collections.size() >= DatabaseFormat.MAX_COLLECTIONS_PER_DATABASE) {
                    throw new DatabaseIntegrityException("Invalid or duplicate collection record");
                }
                CollectionState state = new CollectionState(collectionId, name);
                collections.put(name, state);
                collectionsById.put(collectionId, state);
                nextCollectionId = collectionId == Long.MAX_VALUE ? 0 : collectionId + 1;
            }
            case DOCUMENT_INSERT, DOCUMENT_UPDATE -> {
                DocumentMutation mutation = readDocumentMutation(input);
                CollectionState collection = collectionsById.get(mutation.collectionId());
                if (collection == null) {
                    throw new DatabaseIntegrityException("Document record names an unknown collection");
                }
                CollectionState existingCollection = primaryIdIndex.get(mutation.documentId().value());
                if (type == RecordType.DOCUMENT_INSERT) {
                    if (existingCollection != null) {
                        throw new DatabaseIntegrityException("Duplicate document ID in file");
                    }
                    if (documentCount >= DatabaseFormat.MAX_DOCUMENTS_PER_DATABASE) {
                        throw new DatabaseIntegrityException("Database document limit exceeded in file");
                    }
                    collection.documents.put(mutation.documentId().value(), mutation.document());
                    primaryIdIndex.put(mutation.documentId().value(), collection);
                    documentCount++;
                    advanceNextDocumentId(mutation.documentId().value());
                } else {
                    if (existingCollection != collection) {
                        throw new DatabaseIntegrityException("Update record names an unknown document");
                    }
                    collection.documents.put(mutation.documentId().value(), mutation.document());
                }
            }
            case DOCUMENT_DELETE -> {
                requireRemaining(input, Long.BYTES * 2, "delete record");
                long collectionId = input.getLong();
                long documentId = input.getLong();
                if (input.hasRemaining() || documentId <= 0) {
                    throw new DatabaseIntegrityException("Invalid delete record");
                }
                CollectionState collection = collectionsById.get(collectionId);
                if (collection == null || primaryIdIndex.get(documentId) != collection) {
                    throw new DatabaseIntegrityException("Delete record names an unknown document");
                }
                collection.documents.remove(documentId);
                primaryIdIndex.remove(documentId);
                documentCount--;
            }
        }
    }

    private DocumentMutation readDocumentMutation(ByteBuffer input)
            throws DatabaseIntegrityException {
        requireRemaining(input, Long.BYTES * 2 + Integer.BYTES, "document record");
        long collectionId = input.getLong();
        long documentId = input.getLong();
        if (collectionId <= 0 || documentId <= 0) {
            throw new DatabaseIntegrityException("Document identifiers must be positive");
        }
        int documentLength = input.getInt();
        if (documentLength < 0 || documentLength > DocumentCodec.MAX_DOCUMENT_BYTES) {
            throw new DatabaseIntegrityException("Document length is out of bounds");
        }
        requireRemaining(input, documentLength, "document payload");
        if (input.remaining() != documentLength) {
            throw new DatabaseIntegrityException("Document record contains trailing bytes");
        }
        byte[] encodedDocument = new byte[documentLength];
        input.get(encodedDocument);
        Document document = DocumentCodec.decode(encodedDocument);
        return new DocumentMutation(collectionId, new DocumentId(documentId), document);
    }

    private void recoverIncompleteTail(long lastGoodOffset, long originalSize) throws IOException {
        long discardedBytes = originalSize - lastGoodOffset;
        channel.truncate(lastGoodOffset);
        channel.force(true);
        recoveryReport = new RecoveryReport(discardedBytes);
    }

    private void appendRecord(RecordType type, byte[] payload) throws IOException {
        if (nextSequence <= 0 || nextSequence == Long.MAX_VALUE) {
            throw new DatabaseException("Record sequence space is exhausted");
        }
        long totalLong = (long) DatabaseFormat.MIN_RECORD_SIZE + payload.length;
        if (totalLong > DatabaseFormat.MAX_RECORD_SIZE) {
            throw new DatabaseException("Record exceeds the maximum block size");
        }
        int totalLength = (int) totalLong;
        long currentSize = channel.size();
        if (currentSize > DatabaseFormat.MAX_DATABASE_SIZE_BYTES - totalLength) {
            throw new DatabaseException("Database file size limit reached; compaction is not implemented");
        }
        ByteBuffer output = ByteBuffer.allocate(totalLength).order(ByteOrder.BIG_ENDIAN);
        output.put(DatabaseFormat.RECORD_MAGIC);
        output.put((byte) type.code);
        output.put((byte) 0);
        output.putShort((short) 0);
        output.putInt(totalLength);
        output.putLong(nextSequence);
        output.put(payload);
        output.putInt(DatabaseFormat.crc32c(output.array(), 0, totalLength - Integer.BYTES));
        output.flip();
        try {
            long start = channel.size();
            writeFully(channel, output, start);
            channel.force(true);
            nextSequence++;
        } catch (IOException exception) {
            writeFailed = true;
            throw exception;
        }
    }

    private static byte[] encodeCollectionCreate(long collectionId, String name)
            throws IOException {
        ByteArrayOutputStream bytes = new ByteArrayOutputStream();
        DataOutputStream output = new DataOutputStream(bytes);
        output.writeLong(collectionId);
        DocumentCodec.writeUtf8(output, name, COLLECTION_NAME_MAX_BYTES, "collection name");
        output.flush();
        return bytes.toByteArray();
    }

    private static byte[] encodeDocumentMutation(long collectionId, DocumentId id,
            Document document) throws IOException {
        byte[] encodedDocument = DocumentCodec.encode(document);
        long totalLength = Long.BYTES * 2L + Integer.BYTES + encodedDocument.length;
        if (totalLength > DatabaseFormat.MAX_RECORD_SIZE - DatabaseFormat.MIN_RECORD_SIZE) {
            throw new DatabaseException("Document record exceeds the maximum block size");
        }
        ByteArrayOutputStream bytes = new ByteArrayOutputStream((int) totalLength);
        DataOutputStream output = new DataOutputStream(bytes);
        output.writeLong(collectionId);
        output.writeLong(id.value());
        output.writeInt(encodedDocument.length);
        output.write(encodedDocument);
        output.flush();
        return bytes.toByteArray();
    }

    private static byte[] encodeDocumentDelete(long collectionId, DocumentId id) {
        ByteBuffer payload = ByteBuffer.allocate(Long.BYTES * 2).order(ByteOrder.BIG_ENDIAN);
        payload.putLong(collectionId);
        payload.putLong(id.value());
        return payload.array();
    }

    private CollectionState requireCollection(String name) throws DatabaseException {
        validateCollectionName(name);
        CollectionState collection = collections.get(name);
        if (collection == null) {
            throw new DatabaseException("Collection does not exist: " + name);
        }
        return collection;
    }

    private void advanceNextDocumentId(long usedId) {
        if (usedId >= nextDocumentId) {
            if (usedId == Long.MAX_VALUE) {
                documentIdsExhausted = true;
            } else {
                nextDocumentId = usedId + 1;
            }
        }
    }

    private void checkUsable() throws DatabaseException {
        if (closed) {
            throw new DatabaseException("Database handle is closed");
        }
        if (writeFailed) {
            throw new DatabaseException("A previous append failed; close and reopen the database");
        }
    }

    private static FileLock acquireLock(FileChannel channel, Path path) throws IOException {
        try {
            FileLock lock = channel.tryLock();
            if (lock == null) {
                throw new DatabaseException("Database is already open for writing: " + path);
            }
            return lock;
        } catch (OverlappingFileLockException exception) {
            throw new DatabaseException("Database is already open in this process: " + path, exception);
        }
    }

    private static Path normalizePath(Path path) {
        if (path == null) {
            throw new IllegalArgumentException("path cannot be null");
        }
        return path.toAbsolutePath().normalize();
    }

    private static void validateCollectionName(String name) {
        if (name == null || !name.matches("[A-Za-z_][A-Za-z0-9_-]{0,62}")) {
            throw new IllegalArgumentException(
                    "Collection names must start with a letter or underscore and contain at most 63 ASCII letters, digits, '_' or '-'");
        }
    }

    private static void readFully(FileChannel channel, ByteBuffer buffer, long position)
            throws IOException {
        long offset = position;
        while (buffer.hasRemaining()) {
            int count = channel.read(buffer, offset);
            if (count < 0) {
                throw new DatabaseIntegrityException("Unexpected end of database file");
            }
            if (count == 0) {
                Thread.onSpinWait();
                continue;
            }
            offset += count;
        }
    }

    private static void writeFully(FileChannel channel, ByteBuffer buffer, long position)
            throws IOException {
        long offset = position;
        int emptyWrites = 0;
        while (buffer.hasRemaining()) {
            int count = channel.write(buffer, offset);
            if (count == 0) {
                emptyWrites++;
                if (emptyWrites > MAX_ZERO_LENGTH_WRITES) {
                    throw new IOException("File write made no progress");
                }
                Thread.onSpinWait();
                continue;
            }
            emptyWrites = 0;
            offset += count;
        }
    }

    private static void requireRemaining(ByteBuffer input, int length, String label)
            throws DatabaseIntegrityException {
        if (length < 0 || input.remaining() < length) {
            throw new DatabaseIntegrityException("Truncated " + label);
        }
    }

    private enum RecordType {
        COLLECTION_CREATE(1),
        DOCUMENT_INSERT(2),
        DOCUMENT_UPDATE(3),
        DOCUMENT_DELETE(4);

        private final int code;

        RecordType(int code) {
            this.code = code;
        }

        static RecordType fromCode(int code) throws DatabaseFormatException {
            for (RecordType type : values()) {
                if (type.code == code) {
                    return type;
                }
            }
            throw new DatabaseFormatException("Unknown record kind: " + code);
        }
    }

    private static final class CollectionState {
        private final long id;
        private final String name;
        private final Map<Long, Document> documents = new TreeMap<>();

        private CollectionState(long id, String name) {
            this.id = id;
            this.name = name;
        }
    }

    private record DocumentMutation(long collectionId, DocumentId documentId, Document document) {}
}
