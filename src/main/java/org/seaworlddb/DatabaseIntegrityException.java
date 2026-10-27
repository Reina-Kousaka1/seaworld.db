package org.seaworlddb;

/** Raised when a complete record or its replayed semantics are corrupt. */
public final class DatabaseIntegrityException extends DatabaseException {
    private static final long serialVersionUID = 1L;

    public DatabaseIntegrityException(String message) {
        super(message);
    }
}
