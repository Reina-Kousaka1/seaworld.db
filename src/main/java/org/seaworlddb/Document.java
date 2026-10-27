package org.seaworlddb;

import java.util.Collections;
import java.util.Map;
import java.util.Objects;
import java.util.TreeMap;

/** Immutable top-level document. Its stable identifier is stored separately. */
public final class Document {
    private final Map<String, Value> fields;

    public Document(Map<String, Value> fields) {
        Objects.requireNonNull(fields, "fields");
        TreeMap<String, Value> copy = new TreeMap<>();
        for (Map.Entry<String, Value> entry : fields.entrySet()) {
            copy.put(Objects.requireNonNull(entry.getKey(), "field name"),
                    Objects.requireNonNull(entry.getValue(), "field value"));
        }
        this.fields = Collections.unmodifiableMap(copy);
    }

    public static Document of(Map<String, Value> fields) {
        return new Document(fields);
    }

    public Map<String, Value> fields() {
        return fields;
    }

    public Value get(String fieldName) {
        return fields.get(fieldName);
    }

    public Document with(String fieldName, Value value) {
        Objects.requireNonNull(fieldName, "fieldName");
        Objects.requireNonNull(value, "value");
        TreeMap<String, Value> updated = new TreeMap<>(fields);
        updated.put(fieldName, value);
        return new Document(updated);
    }

    @Override
    public boolean equals(Object other) {
        return other instanceof Document document && fields.equals(document.fields);
    }

    @Override
    public int hashCode() {
        return fields.hashCode();
    }

    @Override
    public String toString() {
        return "Document" + fields;
    }
}
