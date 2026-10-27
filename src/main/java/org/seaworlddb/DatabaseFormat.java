package org.seaworlddb;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.UUID;
import java.util.zip.CRC32C;

/** Constants and explicit header encoding for SeaWorld file format version 1. */
final class DatabaseFormat {
    static final int HEADER_SIZE = 64;
    static final int BLOCK_SIZE = 4_096;
    static final int RECORD_PREFIX_SIZE = 20;
    static final int RECORD_TRAILER_SIZE = Integer.BYTES;
    static final int MIN_RECORD_SIZE = RECORD_PREFIX_SIZE + RECORD_TRAILER_SIZE;
    static final int MAX_RECORD_SIZE = 17 * 1024 * 1024;
    static final long MAX_DATABASE_SIZE_BYTES = 128L * 1024 * 1024;
    static final int MAX_COLLECTIONS_PER_DATABASE = 10_000;
    static final int MAX_DOCUMENTS_PER_DATABASE = 100_000;
    static final short FORMAT_VERSION = 1;

    static final byte[] HEADER_MAGIC = new byte[] {
            'S', 'W', 'D', 'B', '\r', '\n', 0x1a, '\n'
    };
    static final byte[] RECORD_MAGIC = "SWBL".getBytes(StandardCharsets.US_ASCII);

    private DatabaseFormat() {}

    static byte[] encodeHeader(UUID databaseId) {
        ByteBuffer header = ByteBuffer.allocate(HEADER_SIZE).order(ByteOrder.BIG_ENDIAN);
        header.put(HEADER_MAGIC);
        header.putShort(FORMAT_VERSION);
        header.putShort((short) HEADER_SIZE);
        header.putInt(BLOCK_SIZE);
        header.putLong(0L); // Version 1 has no feature flags.
        header.putLong(databaseId.getMostSignificantBits());
        header.putLong(databaseId.getLeastSignificantBits());
        while (header.position() < HEADER_SIZE - Integer.BYTES) {
            header.put((byte) 0);
        }
        header.putInt(crc32c(header.array(), 0, HEADER_SIZE - Integer.BYTES));
        return header.array();
    }

    static Header decodeHeader(byte[] bytes) throws DatabaseException {
        if (bytes.length != HEADER_SIZE) {
            throw new DatabaseFormatException("Database header must be 64 bytes");
        }
        ByteBuffer header = ByteBuffer.wrap(bytes).order(ByteOrder.BIG_ENDIAN);
        byte[] magic = new byte[HEADER_MAGIC.length];
        header.get(magic);
        if (!Arrays.equals(magic, HEADER_MAGIC)) {
            throw new DatabaseFormatException("Invalid SeaWorld database magic bytes");
        }
        int version = Short.toUnsignedInt(header.getShort());
        if (version != FORMAT_VERSION) {
            throw new DatabaseFormatException("Unsupported SeaWorld format version: " + version);
        }
        int headerLength = Short.toUnsignedInt(header.getShort());
        if (headerLength != HEADER_SIZE) {
            throw new DatabaseFormatException("Unsupported database header length: " + headerLength);
        }
        int blockSize = header.getInt();
        if (blockSize != BLOCK_SIZE) {
            throw new DatabaseFormatException("Unsupported database block size: " + blockSize);
        }
        long featureFlags = header.getLong();
        if (featureFlags != 0L) {
            throw new DatabaseFormatException("Database has unsupported required feature flags");
        }
        UUID databaseId = new UUID(header.getLong(), header.getLong());
        for (int index = header.position(); index < HEADER_SIZE - Integer.BYTES; index++) {
            if (bytes[index] != 0) {
                throw new DatabaseFormatException("Reserved header bytes must be zero");
            }
        }
        int storedChecksum = ByteBuffer.wrap(bytes, HEADER_SIZE - Integer.BYTES,
                Integer.BYTES).order(ByteOrder.BIG_ENDIAN).getInt();
        int actualChecksum = crc32c(bytes, 0, HEADER_SIZE - Integer.BYTES);
        if (storedChecksum != actualChecksum) {
            throw new DatabaseIntegrityException("Database header checksum mismatch");
        }
        if (databaseId.getMostSignificantBits() == 0L
                && databaseId.getLeastSignificantBits() == 0L) {
            throw new DatabaseFormatException("Database identifier cannot be zero");
        }
        return new Header(databaseId);
    }

    static int crc32c(byte[] bytes, int offset, int length) {
        CRC32C checksum = new CRC32C();
        checksum.update(bytes, offset, length);
        return (int) checksum.getValue();
    }

    record Header(UUID databaseId) {}
}
