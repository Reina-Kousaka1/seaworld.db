package org.seaworlddb;

import java.util.ArrayList;
import java.util.Collections;
import java.util.Map;
import java.util.Objects;
import java.util.TreeMap;

/** Immutable values supported by SeaWorld's document model. */
public sealed interface Value permits Value.NullValue, Value.BooleanValue,
        Value.IntegerValue, Value.FloatValue, Value.StringValue,
        Value.BinaryValue, Value.ArrayValue, Value.ObjectValue {

    enum NullValue implements Value {
        INSTANCE
    }

    record BooleanValue(boolean value) implements Value {}

    record IntegerValue(long value) implements Value {}

    record FloatValue(double value) implements Value {
        public FloatValue {
            if (!Double.isFinite(value)) {
                throw new IllegalArgumentException("Floating-point values must be finite");
            }
        }
    }

    record StringValue(String value) implements Value {
        public StringValue {
            Objects.requireNonNull(value, "value");
        }
    }

    final class BinaryValue implements Value {
        private final byte[] value;

        public BinaryValue(byte[] value) {
            this.value = Objects.requireNonNull(value, "value").clone();
        }

        public byte[] value() {
            return value.clone();
        }

        @Override
        public boolean equals(Object other) {
            return other instanceof BinaryValue binary
                    && java.util.Arrays.equals(value, binary.value);
        }

        @Override
        public int hashCode() {
            return java.util.Arrays.hashCode(value);
        }
    }

    record ArrayValue(java.util.List<Value> values) implements Value {
        public ArrayValue {
            Objects.requireNonNull(values, "values");
            ArrayList<Value> copy = new ArrayList<>(values.size());
            for (Value value : values) {
                copy.add(Objects.requireNonNull(value, "array value"));
            }
            values = Collections.unmodifiableList(copy);
        }
    }

    record ObjectValue(Map<String, Value> fields) implements Value {
        public ObjectValue {
            Objects.requireNonNull(fields, "fields");
            TreeMap<String, Value> copy = new TreeMap<>();
            for (Map.Entry<String, Value> entry : fields.entrySet()) {
                String key = Objects.requireNonNull(entry.getKey(), "field name");
                Value value = Objects.requireNonNull(entry.getValue(), "field value");
                if (copy.put(key, value) != null) {
                    throw new IllegalArgumentException("Duplicate object field: " + key);
                }
            }
            fields = Collections.unmodifiableMap(copy);
        }
    }

    static Value nullValue() {
        return NullValue.INSTANCE;
    }

    static Value bool(boolean value) {
        return new BooleanValue(value);
    }

    static Value integer(long value) {
        return new IntegerValue(value);
    }

    static Value floating(double value) {
        return new FloatValue(value);
    }

    static Value string(String value) {
        return new StringValue(value);
    }

    static Value binary(byte[] value) {
        return new BinaryValue(value);
    }

    static Value array(Value... values) {
        return new ArrayValue(java.util.List.of(values));
    }

    static Value array(java.util.List<Value> values) {
        return new ArrayValue(values);
    }

    static Value object(Map<String, Value> fields) {
        return new ObjectValue(fields);
    }
}
