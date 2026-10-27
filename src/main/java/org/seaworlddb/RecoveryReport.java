package org.seaworlddb;

/** Details about an incomplete final block discarded while opening a database. */
public record RecoveryReport(long discardedTailBytes) {
    public RecoveryReport {
        if (discardedTailBytes < 0) {
            throw new IllegalArgumentException("discardedTailBytes cannot be negative");
        }
    }

    public boolean recovered() {
        return discardedTailBytes > 0;
    }
}
