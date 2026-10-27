package org.seaworlddb;

import java.io.ByteArrayOutputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Map;
import java.util.TreeMap;

/** Explicit, bounded encoding for SeaWorld documents and values. */
final class DocumentCodec {
    static final int MAX_DOCUMENT_BYTES = 16 * 1024 * 1024;
    static final int MAX_CONTAINER_ITEMS = 100_000;
    static final int MAX_FIELD_NAME_BYTES = 1_024;
    static final int MAX_NESTING_DEPTH = 64;

    private static final int NULL = 0;
    private static final int BOOLEAN = 1;
    private static final int INTEGER = 2;
    private static final int FLOAT = 3;
    private static final int STRING = 4;
    private static final int BINARY = 5;
    private static final int ARRAY = 6;
    private static final int OBJECT = 7;

    private DocumentCodec() {}

    static byte[] encode(Document document) throws DatabaseException {
        try {
            ByteArrayOutputStream bytes = new ByteArrayOutputStream();
            DataOutputStream output = new DataOutputStream(bytes);
            Map<String, Value> fields = document.fields();
            checkItemCount(fields.size(), "document field count");
            output.writeInt(fields.size());
            for (Map.Entry<String, Value> entry : new TreeMap<>(fields).entrySet()) {
                validateFieldNameForWrite(entry.getKey());
                writeUtf8(output, entry.getKey(), MAX_FIELD_NAME_BYTES, "field name");
                writeValue(output, entry.getValue(), 1);
                checkDocumentSize(bytes.size());
            }
            output.flush();
            checkDocumentSize(bytes.size());
            return bytes.toByteArray();
        } catch (IOException exception) {
            if (exception instanceof DatabaseException databaseException) {
                throw databaseException;
            }
            throw new DatabaseException("Could not encode document", exception);
        }
    }

    static Document decode(byte[] encoded) throws DatabaseIntegrityException {
        if (encoded.length > MAX_DOCUMENT_BYTES) {
            throw new DatabaseIntegrityException("Document exceeds the maximum size");
        }
        ByteBuffer input = ByteBuffer.wrap(encoded).order(ByteOrder.BIG_ENDIAN);
        int fieldCount = readCount(input, "document field count");
        TreeMap<String, Value> fields = new TreeMap<>();
        for (int index = 0; index < fieldCount; index++) {
            String name = readUtf8(input, MAX_FIELD_NAME_BYTES, "field name");
            validateFieldName(name);
            Value value = readValue(input, 1);
            if (fields.putIfAbsent(name, value) != null) {
                throw new DatabaseIntegrityException("Duplicate document field: " + name);
            }
        }
        if (input.hasRemaining()) {
            throw new DatabaseIntegrityException("Document payload contains trailing bytes");
        }
        return new Document(fields);
    }

    private static void writeValue(DataOutputStream output, Value value, int depth)
            throws IOException {
        if (depth > MAX_NESTING_DEPTH) {
            throw new DatabaseException("Document nesting exceeds the maximum depth");
        }
        if (value instanceof Value.NullValue) {
            output.writeByte(NULL);
        } else if (value instanceof Value.BooleanValue booleanValue) {
            output.writeByte(BOOLEAN);
            output.writeBoolean(booleanValue.value());
        } else if (value instanceof Value.IntegerValue integerValue) {
            output.writeByte(INTEGER);
            output.writeLong(integerValue.value());
        } else if (value instanceof Value.FloatValue floatValue) {
            output.writeByte(FLOAT);
            output.writeLong(Double.doubleToRawLongBits(floatValue.value()));
        } else if (value instanceof Value.StringValue stringValue) {
            output.writeByte(STRING);
            writeUtf8(output, stringValue.value(), MAX_DOCUMENT_BYTES, "string value");
        } else if (value instanceof Value.BinaryValue binaryValue) {
            output.writeByte(BINARY);
            byte[] binary = binaryValue.value();
            writeLength(output, binary.length, "binary value");
            output.write(binary);
        } else if (value instanceof Value.ArrayValue arrayValue) {
            output.writeByte(ARRAY);
            checkItemCount(arrayValue.values().size(), "array length");
            output.writeInt(arrayValue.values().size());
            for (Value child : arrayValue.values()) {
                writeValue(output, child, depth + 1);
            }
        } else if (value instanceof Value.ObjectValue objectValue) {
            output.writeByte(OBJECT);
            checkItemCount(objectValue.fields().size(), "object field count");
            output.writeInt(objectValue.fields().size());
            for (Map.Entry<String, Value> entry : new TreeMap<>(objectValue.fields()).entrySet()) {
                validateFieldNameForWrite(entry.getKey());
                writeUtf8(output, entry.getKey(), MAX_FIELD_NAME_BYTES, "field name");
                writeValue(output, entry.getValue(), depth + 1);
            }
        } else {
            throw new DatabaseException("Unsupported document value: " + value.getClass());
        }
        if (output.size() > MAX_DOCUMENT_BYTES) {
            throw new DatabaseException("Document exceeds the maximum size");
        }
    }

    private static Value readValue(ByteBuffer input, int depth)
            throws DatabaseIntegrityException {
        if (depth > MAX_NESTING_DEPTH) {
            throw new DatabaseIntegrityException("Document nesting exceeds the maximum depth");
        }
        requireRemaining(input, 1, "value tag");
        int tag = Byte.toUnsignedInt(input.get());
        try {
            return switch (tag) {
                case NULL -> Value.nullValue();
                case BOOLEAN -> {
                    requireRemaining(input, 1, "boolean value");
                    byte value = input.get();
                    if (value != 0 && value != 1) {
                        throw new DatabaseIntegrityException("Boolean value must be 0 or 1");
                    }
                    yield Value.bool(value == 1);
                }
                case INTEGER -> {
                    requireRemaining(input, Long.BYTES, "integer value");
                    yield Value.integer(input.getLong());
                }
                case FLOAT -> {
                    requireRemaining(input, Long.BYTES, "floating-point value");
                    yield Value.floating(Double.longBitsToDouble(input.getLong()));
                }
                case STRING -> Value.string(readUtf8(input, MAX_DOCUMENT_BYTES, "string value"));
                case BINARY -> Value.binary(readBytes(input, MAX_DOCUMENT_BYTES, "binary value"));
                case ARRAY -> {
                    int count = readCount(input, "array length");
                    ArrayList<Value> values = new ArrayList<>(count);
                    for (int index = 0; index < count; index++) {
                        values.add(readValue(input, depth + 1));
                    }
                    yield Value.array(values);
                }
                case OBJECT -> {
                    int count = readCount(input, "object field count");
                    TreeMap<String, Value> fields = new TreeMap<>();
                    for (int index = 0; index < count; index++) {
                        String name = readUtf8(input, MAX_FIELD_NAME_BYTES, "field name");
                        validateFieldName(name);
                        Value value = readValue(input, depth + 1);
                        if (fields.putIfAbsent(name, value) != null) {
                            throw new DatabaseIntegrityException("Duplicate object field: " + name);
                        }
                    }
                    yield Value.object(fields);
                }
                default -> throw new DatabaseIntegrityException("Unknown value tag: " + tag);
            };
        } catch (IllegalArgumentException exception) {
            throw new DatabaseIntegrityException("Invalid value encoding: " + exception.getMessage());
        }
    }

    static void writeUtf8(DataOutputStream output, String value, int maxBytes, String label)
            throws IOException {
        byte[] bytes = encodeUtf8(value, maxBytes, label);
        output.writeInt(bytes.length);
        output.write(bytes);
    }

    static byte[] encodeUtf8(String value, int maxBytes, String label) throws DatabaseException {
        try {
            ByteBuffer encoded = StandardCharsets.UTF_8.newEncoder()
                    .onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT)
                    .encode(java.nio.CharBuffer.wrap(value));
            if (encoded.remaining() > maxBytes) {
                throw new DatabaseException(label + " exceeds the maximum UTF-8 size");
            }
            byte[] bytes = new byte[encoded.remaining()];
            encoded.get(bytes);
            return bytes;
        } catch (CharacterCodingException exception) {
            throw new DatabaseException(label + " contains invalid Unicode", exception);
        }
    }

    static String readUtf8(ByteBuffer input, int maxBytes, String label)
            throws DatabaseIntegrityException {
        byte[] bytes = readBytes(input, maxBytes, label);
        try {
            return StandardCharsets.UTF_8.newDecoder()
                    .onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT)
                    .decode(ByteBuffer.wrap(bytes))
                    .toString();
        } catch (CharacterCodingException exception) {
            throw new DatabaseIntegrityException(label + " is not valid UTF-8");
        }
    }

    static byte[] readBytes(ByteBuffer input, int maxBytes, String label)
            throws DatabaseIntegrityException {
        requireRemaining(input, Integer.BYTES, label + " length");
        int length = input.getInt();
        if (length < 0 || length > maxBytes) {
            throw new DatabaseIntegrityException(label + " length is out of bounds");
        }
        requireRemaining(input, length, label);
        byte[] bytes = new byte[length];
        input.get(bytes);
        return bytes;
    }

    private static int readCount(ByteBuffer input, String label)
            throws DatabaseIntegrityException {
        requireRemaining(input, Integer.BYTES, label);
        int count = input.getInt();
        if (count < 0 || count > MAX_CONTAINER_ITEMS) {
            throw new DatabaseIntegrityException(label + " is out of bounds");
        }
        return count;
    }

    private static void writeLength(DataOutputStream output, int length, String label)
            throws IOException {
        if (length < 0 || length > MAX_DOCUMENT_BYTES) {
            throw new DatabaseException(label + " length is out of bounds");
        }
        output.writeInt(length);
    }

    private static void checkItemCount(int count, String label) throws DatabaseException {
        if (count < 0 || count > MAX_CONTAINER_ITEMS) {
            throw new DatabaseException(label + " is out of bounds");
        }
    }

    private static void checkDocumentSize(int size) throws DatabaseException {
        if (size > MAX_DOCUMENT_BYTES) {
            throw new DatabaseException("Document exceeds the maximum size");
        }
    }

    private static void validateFieldName(String name) throws DatabaseIntegrityException {
        if (name.isEmpty() || name.indexOf('\0') >= 0) {
            throw new DatabaseIntegrityException("Field name is empty or contains NUL");
        }
    }

    private static void validateFieldNameForWrite(String name) throws DatabaseException {
        if (name.isEmpty() || name.indexOf('\0') >= 0) {
            throw new DatabaseException("Field name is empty or contains NUL");
        }
    }

    private static void requireRemaining(ByteBuffer input, int required, String label)
            throws DatabaseIntegrityException {
        if (required < 0 || input.remaining() < required) {
            throw new DatabaseIntegrityException("Truncated " + label);
        }
    }
}
