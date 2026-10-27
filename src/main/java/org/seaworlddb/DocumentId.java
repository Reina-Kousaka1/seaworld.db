package org.seaworlddb;

/** Stable positive document identifier within a database. */
public record DocumentId(long value) implements Comparable<DocumentId> {
    public DocumentId {
        if (value <= 0) {
            throw new IllegalArgumentException("Document IDs must be positive");
        }
    }

    @Override
    public int compareTo(DocumentId other) {
        return Long.compare(value, other.value);
    }
}
