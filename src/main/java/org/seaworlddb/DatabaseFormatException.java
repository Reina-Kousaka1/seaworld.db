package org.seaworlddb;

/** Raised when a file does not match a supported SeaWorld format. */
public final class DatabaseFormatException extends DatabaseException {
    private static final long serialVersionUID = 1L;

    public DatabaseFormatException(String message) {
        super(message);
    }
}
