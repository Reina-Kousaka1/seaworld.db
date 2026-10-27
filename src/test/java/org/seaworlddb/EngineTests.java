package org.seaworlddb;

import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.channels.FileChannel;
import java.nio.charset.StandardCharsets;
import java.nio.file.FileAlreadyExistsException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.UUID;
import java.util.zip.CRC32C;

/** Dependency-free integration tests for the first persistent engine vertical. */
public final class EngineTests {
    private EngineTests() {}

    public static void main(String[] arguments) throws Exception {
        List<TestCase> tests = List.of(
                new TestCase("create database file and header", EngineTests::createDatabase),
                new TestCase("reopen empty database", EngineTests::reopenEmptyDatabase),
                new TestCase("create and reopen collection", EngineTests::collectionPersists),
                new TestCase("insert and read document", EngineTests::insertAndRead),
                new TestCase("update document persists", EngineTests::updatePersists),
                new TestCase("delete document persists", EngineTests::deletePersists),
                new TestCase("nested objects and arrays persist", EngineTests::nestedValuesPersist),
                new TestCase("multiple collections stay separate", EngineTests::multipleCollections),
                new TestCase("data survives full close and reopen", EngineTests::restartPersistence),
                new TestCase("invalid magic is rejected", EngineTests::invalidHeader),
                new TestCase("truncated header is rejected", EngineTests::truncatedHeader),
                new TestCase("incomplete tail is reported and recovered", EngineTests::recoverIncompleteTail),
                new TestCase("incomplete declared block is reported and recovered", EngineTests::recoverIncompleteBlock),
                new TestCase("duplicate document IDs are rejected", EngineTests::duplicateIds),
                new TestCase("primary ID index survives replay", EngineTests::primaryIndexCorrectness),
                new TestCase("complete block checksum mismatch is rejected", EngineTests::checksumMismatch),
                new TestCase("invalid UTF-8 is rejected", EngineTests::invalidUtf8),
                new TestCase("oversized block length is rejected", EngineTests::oversizedBlock),
                new TestCase("collection names cannot contain path traversal", EngineTests::pathTraversalName),
                new TestCase("excessively nested documents are rejected", EngineTests::nestedValueLimit),
                new TestCase("create never overwrites an existing file", EngineTests::noOverwrite),
                new TestCase("one process cannot open a second writer", EngineTests::exclusiveLock));

        int passed = 0;
        int failed = 0;
        for (TestCase test : tests) {
            try {
                test.body().run();
                passed++;
                System.out.println("[PASS] " + test.name());
            } catch (Throwable failure) {
                failed++;
                System.err.println("[FAIL] " + test.name() + ": " + failure);
                failure.printStackTrace(System.err);
            }
        }
        System.out.printf("Tests: passed=%d failed=%d skipped=0%n", passed, failed);
        if (failed != 0) {
            throw new AssertionError(failed + " test(s) failed");
        }
    }

    private static void createDatabase() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("empty.seaworld.db");
            UUID identifier;
            try (Database database = Database.create(path)) {
                identifier = database.databaseId();
                equal(64L, Files.size(path), "new file should contain its 64-byte header");
            }
            check(identifier.getMostSignificantBits() != 0 || identifier.getLeastSignificantBits() != 0,
                    "database identifier must not be zero");
            try (Database database = Database.open(path)) {
                check(database.databaseId() != null,
                        "successful open validates magic, version, header length, flags and checksum");
            }
        });
    }

    private static void reopenEmptyDatabase() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("empty.seaworld.db");
            UUID original;
            try (Database database = Database.create(path)) {
                original = database.databaseId();
            }
            try (Database database = Database.open(path)) {
                equal(original, database.databaseId(), "database identifier must survive reopen");
                equal(List.of(), database.collectionNames(), "new database should have no collections");
            }
        });
    }

    private static void collectionPersists() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("collections.seaworld.db");
            try (Database database = Database.create(path)) {
                equal(new CollectionId(1), database.createCollection("users"), "first collection ID");
            }
            try (Database database = Database.open(path)) {
                equal(List.of("users"), database.collectionNames(), "collection must replay");
            }
        });
    }

    private static void insertAndRead() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("insert.seaworld.db");
            Document expected = document("username", Value.string("Charlotte"));
            DocumentId id;
            try (Database database = Database.create(path)) {
                database.createCollection("users");
                id = database.insert("users", expected);
                equal(new DocumentId(1), id, "first generated document ID");
                equal(Optional.of(expected), database.get("users", id), "read inserted document");
            }
            try (Database database = Database.open(path)) {
                equal(Optional.of(expected), database.get("users", id), "read document after reopen");
            }
        });
    }

    private static void updatePersists() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("update.seaworld.db");
            DocumentId id;
            Document updated = document("credits", Value.integer(1250));
            try (Database database = Database.create(path)) {
                database.createCollection("users");
                id = database.insert("users", document("credits", Value.integer(100)));
                check(database.update("users", id, updated), "existing document should update");
                check(!database.update("users", new DocumentId(999), updated),
                        "missing document should not update");
            }
            try (Database database = Database.open(path)) {
                equal(Optional.of(updated), database.get("users", id), "updated document should replay");
            }
        });
    }

    private static void deletePersists() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("delete.seaworld.db");
            DocumentId id;
            try (Database database = Database.create(path)) {
                database.createCollection("users");
                id = database.insert("users", document("username", Value.string("Charlotte")));
                check(database.delete("users", id), "existing document should delete");
                check(!database.delete("users", id), "second delete should report missing document");
            }
            try (Database database = Database.open(path)) {
                equal(Optional.empty(), database.get("users", id), "deleted document must stay absent");
                equal(List.of(), database.documentIds("users"), "deleted ID must leave primary index");
            }
        });
    }

    private static void nestedValuesPersist() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("nested.seaworld.db");
            Map<String, Value> profile = Map.of(
                    "career", Value.string("ballet"),
                    "level", Value.integer(17));
            Map<String, Value> input = new LinkedHashMap<>();
            input.put("username", Value.string("Charlotte"));
            input.put("credits", Value.integer(1250));
            input.put("profile", Value.object(profile));
            input.put("inventory", Value.array(Value.string("pink_leotard"),
                    Value.string("ballet_shoes"), Value.nullValue(), Value.bool(true)));
            input.put("payload", Value.binary(new byte[] {0, 1, -1}));
            Document expected = new Document(input);
            DocumentId id;
            try (Database database = Database.create(path)) {
                database.createCollection("users");
                id = database.insert("users", expected);
            }
            try (Database database = Database.open(path)) {
                equal(Optional.of(expected), database.get("users", id),
                        "nested objects, arrays, null, boolean and binary values must persist");
            }
        });
    }

    private static void multipleCollections() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("multi.seaworld.db");
            DocumentId userId;
            DocumentId guildId;
            try (Database database = Database.create(path)) {
                database.createCollection("users");
                database.createCollection("guilds");
                userId = database.insert("users", document("name", Value.string("Charlotte")));
                guildId = database.insert("guilds", document("name", Value.string("Moonlight")));
                check(!database.get("guilds", userId).isPresent(), "IDs are scoped to their collection on lookup");
            }
            try (Database database = Database.open(path)) {
                equal(List.of("guilds", "users"), database.collectionNames(), "both collections replay");
                check(database.get("users", userId).isPresent(), "user remains in users");
                check(database.get("guilds", guildId).isPresent(), "guild remains in guilds");
            }
        });
    }

    private static void restartPersistence() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("restart.seaworld.db");
            Document expected = document("value", Value.floating(3.5));
            DocumentId id;
            try (Database database = Database.create(path)) {
                database.createCollection("values");
                id = database.insert("values", expected);
            }
            try (Database reopened = Database.open(path)) {
                equal(Optional.of(expected), reopened.get("values", id), "first full process-like reopen");
            }
            try (Database reopenedAgain = Database.open(path)) {
                equal(Optional.of(expected), reopenedAgain.get("values", id), "repeated reopen");
            }
        });
    }

    private static void invalidHeader() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("invalid.seaworld.db");
            try (Database database = Database.create(path)) {
                check(database.databaseId() != null, "database should be created");
            }
            try (FileChannel channel = FileChannel.open(path, StandardOpenOption.WRITE)) {
                channel.write(ByteBuffer.wrap(new byte[] {'X'}), 0);
                channel.force(true);
            }
            expect(DatabaseFormatException.class, () -> Database.open(path));
        });
    }

    private static void truncatedHeader() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("truncated.seaworld.db");
            Files.write(path, new byte[] {'S', 'W', 'D', 'B', 1, 0, 0, 0});
            expect(DatabaseFormatException.class, () -> Database.open(path));
        });
    }

    private static void recoverIncompleteTail() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("tail.seaworld.db");
            DocumentId id;
            try (Database database = Database.create(path)) {
                database.createCollection("users");
                id = database.insert("users", document("name", Value.string("Charlotte")));
            }
            Files.write(path, new byte[] {'S', 'W', 'B'}, StandardOpenOption.APPEND);
            try (Database database = Database.open(path)) {
                equal(3L, database.recoveryReport().discardedTailBytes(), "recovery should report discarded tail");
                check(database.get("users", id).isPresent(), "earlier complete records remain readable");
            }
            try (Database database = Database.open(path)) {
                equal(new RecoveryReport(0), database.recoveryReport(), "recovery should be repeatable");
                check(database.get("users", id).isPresent(), "data remains readable after repeated reopen");
            }
        });
    }

    private static void recoverIncompleteBlock() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("incomplete-block.seaworld.db");
            DocumentId id;
            try (Database database = Database.create(path)) {
                database.createCollection("users");
                id = database.insert("users", document("name", Value.string("Charlotte")));
            }
            appendIncompleteRecordPrefix(path, 128, 3);
            try (Database database = Database.open(path)) {
                equal(DatabaseFormat.RECORD_PREFIX_SIZE,
                        (int) database.recoveryReport().discardedTailBytes(),
                        "recovery should discard the incomplete declared block");
                check(database.get("users", id).isPresent(), "earlier complete records remain readable");
            }
            try (Database database = Database.open(path)) {
                equal(new RecoveryReport(0), database.recoveryReport(), "block recovery should be repeatable");
            }
        });
    }

    private static void duplicateIds() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("duplicate.seaworld.db");
            try (Database database = Database.create(path)) {
                database.createCollection("users");
                database.createCollection("guilds");
                DocumentId id = new DocumentId(42);
                database.insert("users", id, document("name", Value.string("Charlotte")));
                expect(DatabaseException.class,
                        () -> database.insert("users", id, document("name", Value.string("Other"))));
                expect(DatabaseException.class,
                        () -> database.insert("guilds", id, document("name", Value.string("Other"))));
            }
            try (Database database = Database.open(path)) {
                equal(Optional.of(document("name", Value.string("Charlotte"))),
                        database.get("users", new DocumentId(42)), "failed duplicate inserts must not alter data");
            }
        });
    }

    private static void primaryIndexCorrectness() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("index.seaworld.db");
            ArrayList<DocumentId> userIds = new ArrayList<>();
            ArrayList<DocumentId> groupIds = new ArrayList<>();
            try (Database database = Database.create(path)) {
                database.createCollection("users");
                database.createCollection("groups");
                for (int index = 0; index < 100; index++) {
                    userIds.add(database.insert("users", document("ordinal", Value.integer(index))));
                    groupIds.add(database.insert("groups", document("ordinal", Value.integer(index))));
                }
            }
            try (Database database = Database.open(path)) {
                for (int index = 0; index < 100; index++) {
                    equal(Optional.of(document("ordinal", Value.integer(index))),
                            database.get("users", userIds.get(index)), "user primary-index lookup " + index);
                    equal(Optional.of(document("ordinal", Value.integer(index))),
                            database.get("groups", groupIds.get(index)), "group primary-index lookup " + index);
                }
            }
        });
    }

    private static void checksumMismatch() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("checksum.seaworld.db");
            try (Database database = Database.create(path)) {
                database.createCollection("users");
            }
            try (FileChannel channel = FileChannel.open(path, StandardOpenOption.WRITE)) {
                long offset = DatabaseFormat.HEADER_SIZE + DatabaseFormat.RECORD_PREFIX_SIZE;
                channel.write(ByteBuffer.wrap(new byte[] {0x7f}), offset);
                channel.force(true);
            }
            expect(DatabaseIntegrityException.class, () -> Database.open(path));
        });
    }

    private static void invalidUtf8() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("utf8.seaworld.db");
            try (Database database = Database.create(path)) {
                check(database.databaseId() != null, "database should be created");
            }
            byte[] payload = ByteBuffer.allocate(Long.BYTES + Integer.BYTES + 1)
                    .order(ByteOrder.BIG_ENDIAN)
                    .putLong(1)
                    .putInt(1)
                    .put((byte) 0xff)
                    .array();
            appendValidRecord(path, 1, 1, payload);
            expect(DatabaseIntegrityException.class, () -> Database.open(path));
        });
    }

    private static void oversizedBlock() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("oversized.seaworld.db");
            try (Database database = Database.create(path)) {
                check(database.databaseId() != null, "database should be created");
            }
            ByteBuffer prefix = ByteBuffer.allocate(DatabaseFormat.RECORD_PREFIX_SIZE)
                    .order(ByteOrder.BIG_ENDIAN);
            prefix.put(DatabaseFormat.RECORD_MAGIC);
            prefix.put((byte) 1);
            prefix.put((byte) 0);
            prefix.putShort((short) 0);
            prefix.putInt(DatabaseFormat.MAX_RECORD_SIZE + 1);
            prefix.putLong(1);
            Files.write(path, prefix.array(), StandardOpenOption.APPEND);
            expect(DatabaseFormatException.class, () -> Database.open(path));
        });
    }

    private static void pathTraversalName() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("path-name.seaworld.db");
            try (Database database = Database.create(path)) {
                expect(IllegalArgumentException.class, () -> database.createCollection("../outside"));
                equal(List.of(), database.collectionNames(), "invalid name must not create a collection");
            }
            equal(64L, Files.size(path), "invalid name must not append a record");
        });
    }

    private static void nestedValueLimit() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("depth.seaworld.db");
            Value nested = Value.integer(1);
            for (int index = 0; index < DocumentCodec.MAX_NESTING_DEPTH + 2; index++) {
                nested = Value.object(Map.of("child", nested));
            }
            Document tooDeep = document("nested", nested);
            try (Database database = Database.create(path)) {
                database.createCollection("values");
                expect(DatabaseException.class,
                        () -> database.insert("values", tooDeep));
                equal(List.of(), database.documentIds("values"), "oversized nesting must not be persisted");
            }
            try (Database database = Database.open(path)) {
                equal(List.of(), database.documentIds("values"), "reopen must preserve empty collection");
            }
        });
    }

    private static void noOverwrite() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("existing.seaworld.db");
            try (Database database = Database.create(path)) {
                check(database.databaseId() != null, "database should be created");
            }
            expect(FileAlreadyExistsException.class, () -> Database.create(path));
        });
    }

    private static void exclusiveLock() throws Exception {
        withDirectory(directory -> {
            Path path = directory.resolve("locked.seaworld.db");
            try (Database database = Database.create(path)) {
                check(database.databaseId() != null, "database should be created");
                expect(DatabaseException.class, () -> Database.open(path));
            }
            try (Database reopened = Database.open(path)) {
                check(reopened.recoveryReport().discardedTailBytes() == 0, "lock release leaves a clean file");
            }
        });
    }

    private static void appendValidRecord(Path path, int type, long sequence, byte[] payload)
            throws IOException {
        int totalLength = DatabaseFormat.MIN_RECORD_SIZE + payload.length;
        ByteBuffer block = ByteBuffer.allocate(totalLength).order(ByteOrder.BIG_ENDIAN);
        block.put(DatabaseFormat.RECORD_MAGIC);
        block.put((byte) type);
        block.put((byte) 0);
        block.putShort((short) 0);
        block.putInt(totalLength);
        block.putLong(sequence);
        block.put(payload);
        block.putInt(DatabaseFormat.crc32c(block.array(), 0, totalLength - Integer.BYTES));
        Files.write(path, block.array(), StandardOpenOption.APPEND);
    }

    private static void appendIncompleteRecordPrefix(Path path, int totalLength, long sequence)
            throws IOException {
        ByteBuffer prefix = ByteBuffer.allocate(DatabaseFormat.RECORD_PREFIX_SIZE)
                .order(ByteOrder.BIG_ENDIAN);
        prefix.put(DatabaseFormat.RECORD_MAGIC);
        prefix.put((byte) 2);
        prefix.put((byte) 0);
        prefix.putShort((short) 0);
        prefix.putInt(totalLength);
        prefix.putLong(sequence);
        Files.write(path, prefix.array(), StandardOpenOption.APPEND);
    }

    private static Document document(String key, Value value) {
        return new Document(Map.of(key, value));
    }

    private static void withDirectory(ThrowingConsumer<Path> body) throws Exception {
        Path directory = Files.createTempDirectory("seaworld-db-tests-");
        try {
            body.accept(directory);
        } finally {
            try (var paths = Files.walk(directory)) {
                for (Path path : paths.sorted(java.util.Comparator.reverseOrder()).toList()) {
                    Files.deleteIfExists(path);
                }
            }
        }
    }

    private static void check(boolean condition, String message) {
        if (!condition) {
            throw new AssertionError(message);
        }
    }

    private static void equal(Object expected, Object actual, String message) {
        if (!java.util.Objects.equals(expected, actual)) {
            throw new AssertionError(message + " (expected " + expected + ", got " + actual + ")");
        }
    }

    private static <T extends Throwable> T expect(Class<T> expectedType, ThrowingRunnable body)
            throws Exception {
        try {
            body.run();
        } catch (Throwable failure) {
            if (expectedType.isInstance(failure)) {
                return expectedType.cast(failure);
            }
            throw new AssertionError("Expected " + expectedType.getSimpleName()
                    + " but got " + failure, failure);
        }
        throw new AssertionError("Expected " + expectedType.getSimpleName() + " but nothing was thrown");
    }

    @FunctionalInterface
    private interface ThrowingRunnable {
        void run() throws Exception;
    }

    @FunctionalInterface
    private interface ThrowingConsumer<T> {
        void accept(T value) throws Exception;
    }

    private record TestCase(String name, ThrowingRunnable body) {}
}
