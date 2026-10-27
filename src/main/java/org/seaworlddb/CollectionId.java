package org.seaworlddb;

/** Stable collection identifier within a database. */
public record CollectionId(long value) {
    public CollectionId {
        if (value <= 0) {
            throw new IllegalArgumentException("Collection IDs must be positive");
        }
    }
}
