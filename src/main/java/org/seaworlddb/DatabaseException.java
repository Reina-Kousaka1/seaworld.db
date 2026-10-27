package org.seaworlddb;

import java.io.IOException;

/** Base checked exception for invalid or damaged SeaWorld database files. */
public class DatabaseException extends IOException {
    private static final long serialVersionUID = 1L;

    public DatabaseException(String message) {
        super(message);
    }

    public DatabaseException(String message, Throwable cause) {
        super(message, cause);
    }
}
