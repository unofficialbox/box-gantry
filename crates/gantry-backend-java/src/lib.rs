//! The Java backend: lowering + printer (FR-6, TR-Java, D-164).
//!
//! Generates the SDK's **model layer** — the first Java slice (D-170). The IR
//! module tree lowers directly to a Java package tree (Java has real packages,
//! so no flattening/mangling like Apex): one package per IR module, one `.java`
//! file per declaration (Java's one-public-type-per-file rule).
//!
//! - **Structs** → immutable `record`s (finalized in Java 16), the clean fit the
//!   D-164 roadmap calls out — no getters/setters/builders ceremony.
//! - **Optionality** maps to distinct type shapes so the absent/null/value
//!   tri-state (D-110) stays visible. Java has no native absent-vs-null
//!   distinction, so a `Tristate<T>` wrapper (emitted into `com.box.sdk.core`)
//!   carries all three states; a plain optional is `java.util.Optional<T>`; a
//!   nullable-but-present field is a bare (nullable) reference.
//! - **Open enums** (Box's extensible enums, D-012) → a `record` over the raw
//!   `String` with the known values as constants, so an unknown value
//!   round-trips untouched; **closed enums** → a real `enum` carrying each
//!   value's wire spelling.
//! - **Aliases** are resolved through (Java has no type alias). A
//!   discriminated **union** whose variants are all same-package,
//!   discriminator-carrying structs lowers to a `sealed interface … permits`
//!   over the variant records (D-171) — Java's natural `oneOf` shape; anything
//!   else stays a structural `record(Object value)` fallback.
//! - **Serialization** (D-172): Java's standard library ships no JSON, so a
//!   hand-written, dependency-free codec (`com.box.sdk.core.Json`, parse +
//!   write of the `Object` tree) plus a `toJson`/`fromJson` pair on every model
//!   type carries the wire mapping. Unions decode by pattern-matching `switch`
//!   on the discriminator (an open union routes an unknown tag to its `Unknown`
//!   catch-all, VR-4); the tri-state (D-110) drives absent-omit / null / value.
//!
//! Output is deterministic (FR-6.2, sorted by path) and verified by the real
//! toolchain: `javac -Xlint:all -Werror` compiles the whole generated tree
//! (VR-1.6), the Java analogue of `go build` / `cargo check` / `tsc --noEmit`,
//! and a `java` round-trip exercises the codec end to end.

mod docs;
mod managers;
mod models;
mod ship;
mod tests;

pub use docs::generate_docs;
pub use managers::generate_managers;
pub use models::generate_models;
pub use ship::generate_ship;
pub use tests::generate_tests;

/// One generated file, path relative to the SDK project root.
#[derive(Debug)]
pub struct GeneratedFile {
    pub path: String,
    pub content: String,
}

/// Provenance stamped into the generated SDK for traceability (NF-7): the
/// engine version that produced it and the fingerprint of the input specs.
#[derive(Debug, Clone)]
pub struct BuildInfo {
    /// The box-gantry engine version (workspace version).
    pub engine: String,
    /// The input spec-set fingerprint (`SpecSet::fingerprint`).
    pub spec_fingerprint: String,
}

impl BuildInfo {
    /// Build info for the current engine over a given spec fingerprint.
    pub fn new(spec_fingerprint: impl Into<String>) -> Self {
        Self {
            engine: env!("CARGO_PKG_VERSION").to_string(),
            spec_fingerprint: spec_fingerprint.into(),
        }
    }
}

/// The root package every generated type lives under.
pub(crate) const ROOT_PKG: &str = "com.box.sdk";
/// The package the model tree hangs off (`com.box.sdk.model.<module>`).
pub(crate) const MODEL_PKG: &str = "com.box.sdk.model";
/// The package the hand-written support types live in.
pub(crate) const CORE_PKG: &str = "com.box.sdk.core";
/// The package the per-tag manager classes live in.
pub(crate) const MANAGERS_PKG: &str = "com.box.sdk.managers";
/// The package the runtime-contract surface (the stub / real runtime) lives in.
pub(crate) const RUNTIME_PKG: &str = "com.box.sdk.runtime";
/// The package the generation-side helper (`Internal`) lives in.
pub(crate) const INTERNAL_PKG: &str = "com.box.sdk.internal";
/// The Maven/Gradle source-root prefix every `.java` file sits under.
pub(crate) const SRC_ROOT: &str = "src/main/java";

/// Generate the SDK source tree for a verified program, stamped with the build
/// provenance (NF-7).
///
/// Takes the manifest so synthesis keys off capability axes, never the language
/// name (FR-4.2); this slice reads only `manifest.key`.
pub fn generate(
    analysis: &gantry_sema::Analysis<'_>,
    manifest: &gantry_manifest::CapabilityManifest,
    build: &BuildInfo,
) -> Vec<GeneratedFile> {
    let mut files = vec![
        GeneratedFile {
            path: java_path(CORE_PKG, "Tristate"),
            content: TRISTATE.to_string(),
        },
        GeneratedFile {
            path: java_path(CORE_PKG, "Json"),
            content: JSON.to_string(),
        },
        GeneratedFile {
            path: java_path(ROOT_PKG, "BuildInfo"),
            content: build_info_java(manifest, build),
        },
        // The runtime-contract stub (FR-5.3): generated code compiles against
        // it without the real runtime, and it can't drift from the declared
        // surface because both come from the same contract data (FR-5.2).
        GeneratedFile {
            path: java_path(RUNTIME_PKG, "Runtime"),
            content: gantry_contract::java_stubs(&gantry_contract::V1, manifest),
        },
    ];
    files.extend(generate_models(analysis, build));
    files.extend(generate_managers(analysis, build));
    files.extend(generate_docs(analysis));
    files.extend(generate_tests(analysis));
    files.extend(generate_ship(build));
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// The repo-relative path of a `.java` file for `type` in `package`
/// (`com.box.sdk.core` + `Tristate` → `src/main/java/com/box/sdk/core/Tristate.java`).
pub(crate) fn java_path(package: &str, type_name: &str) -> String {
    format!("{SRC_ROOT}/{}/{type_name}.java", package.replace('.', "/"))
}

/// Build provenance for the generated SDK (NF-7): a final, uninstantiable class
/// of constants the shipped SDK can report about its own origin.
fn build_info_java(manifest: &gantry_manifest::CapabilityManifest, build: &BuildInfo) -> String {
    format!(
        "// Code generated by box-gantry {engine} (spec {fingerprint}). DO NOT EDIT.\n\
         package {ROOT_PKG};\n\
         \n\
         /** Build provenance for this generated SDK (NF-7). */\n\
         public final class BuildInfo {{\n\
         \x20   private BuildInfo() {{}}\n\
         \n\
         \x20   /** The box-gantry engine version that generated this SDK. */\n\
         \x20   public static final String ENGINE = {engine:?};\n\
         \n\
         \x20   /** Fingerprint of the input spec set. */\n\
         \x20   public static final String SPEC_FINGERPRINT = {fingerprint:?};\n\
         \n\
         \x20   /** The target language key (FR-4). */\n\
         \x20   public static final String TARGET = {target:?};\n\
         }}\n",
        engine = build.engine,
        fingerprint = build.spec_fingerprint,
        target = manifest.key,
    )
}

/// The tri-state wrapper (D-110): Java has no native absent-vs-null
/// distinction, so `Optional<Nullable<T>>` fields carry all three states here
/// (the documented platform shape, like Go's `Nullable[T]`).
const TRISTATE: &str = "\
// Code generated by box-gantry. DO NOT EDIT.
package com.box.sdk.core;

import java.util.Objects;

/**
 * The absent / null / present tri-state (D-110). Java has no native
 * absent-vs-null distinction, so a field that may be omitted, sent as JSON
 * {@code null}, or sent with a value carries this explicit wrapper.
 *
 * @param <T> the value type when present
 */
public record Tristate<T>(State state, T value) {
    /**
     * Enforces the state/value contract so serialization can rely on it: a
     * {@code PRESENT} tri-state carries a non-null value; {@code ABSENT} and
     * {@code NULL} carry none. Use the {@code absent()}/{@code ofNull()}/{@code
     * of(value)} factories rather than this constructor.
     */
    public Tristate {
        Objects.requireNonNull(state, \"state\");
        if (state == State.PRESENT) {
            Objects.requireNonNull(value, \"a PRESENT tri-state requires a value\");
        } else if (value != null) {
            throw new IllegalArgumentException(state + \" tri-state must not carry a value\");
        }
    }

    /** Which of the three states a tri-state field holds. */
    public enum State {
        ABSENT,
        NULL,
        PRESENT
    }

    /** A field that was omitted entirely. */
    public static <T> Tristate<T> absent() {
        return new Tristate<>(State.ABSENT, null);
    }

    /** A field explicitly sent as JSON {@code null}. */
    public static <T> Tristate<T> ofNull() {
        return new Tristate<>(State.NULL, null);
    }

    /** A field sent with a concrete value. */
    public static <T> Tristate<T> of(T value) {
        return new Tristate<>(State.PRESENT, value);
    }

    /** Whether the field was omitted entirely. */
    public boolean isAbsent() {
        return state == State.ABSENT;
    }

    /** Whether the field was explicitly sent as JSON {@code null}. */
    public boolean isNull() {
        return state == State.NULL;
    }

    /** Whether the field was sent with a concrete value. */
    public boolean isPresent() {
        return state == State.PRESENT;
    }
}
";

/// The dependency-free JSON codec (D-172). Java's standard library ships no
/// JSON, so the generated `toJson`/`fromJson` methods build and consume a plain
/// `Object` tree — `Map<String, Object>` / `List<Object>` / `String` / `Long` /
/// `Double` / `Boolean` / `null` — and this runtime parses to and writes from
/// that tree. A `LinkedHashMap` preserves field order so output is
/// deterministic (FR-6.2). The generic `encode*`/`decode*`/`as*` helpers keep
/// the generated per-type code type-safe (no unchecked casts leak past here).
const JSON: &str = "\
// Code generated by box-gantry. DO NOT EDIT.
package com.box.sdk.core;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.function.Function;

/**
 * A minimal, dependency-free JSON reader/writer over a plain {@code Object}
 * tree (D-172). Java's standard library ships no JSON, so the generated model
 * codecs ({@code toJson}/{@code fromJson}) build and consume this tree and rely
 * on the helpers here. Parsed objects are {@link LinkedHashMap}s and arrays are
 * {@link ArrayList}s, so written output preserves key order (determinism,
 * FR-6.2).
 */
public final class Json {
    private Json() {}

    // ---------------------------------------------------------------- writing

    /** Serialize an {@code Object} tree to a JSON string. */
    public static String write(Object value) {
        StringBuilder out = new StringBuilder();
        writeValue(out, value);
        return out.toString();
    }

    private static void writeValue(StringBuilder out, Object value) {
        switch (value) {
            case null -> out.append(\"null\");
            case String s -> writeString(out, s);
            case Boolean b -> out.append(b.booleanValue() ? \"true\" : \"false\");
            case Integer i -> out.append(i.intValue());
            case Long l -> out.append(l.longValue());
            case Double d -> out.append(Double.toString(d));
            case Map<?, ?> m -> writeObject(out, m);
            case List<?> list -> writeArray(out, list);
            default -> throw new IllegalArgumentException(
                \"cannot serialize \" + value.getClass().getName());
        }
    }

    private static void writeObject(StringBuilder out, Map<?, ?> map) {
        out.append('{');
        boolean first = true;
        for (Map.Entry<?, ?> entry : map.entrySet()) {
            if (!first) {
                out.append(',');
            }
            first = false;
            writeString(out, String.valueOf(entry.getKey()));
            out.append(':');
            writeValue(out, entry.getValue());
        }
        out.append('}');
    }

    private static void writeArray(StringBuilder out, List<?> list) {
        out.append('[');
        for (int i = 0; i < list.size(); i++) {
            if (i > 0) {
                out.append(',');
            }
            writeValue(out, list.get(i));
        }
        out.append(']');
    }

    private static void writeString(StringBuilder out, String s) {
        out.append('\"');
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            switch (c) {
                case '\"' -> out.append(\"\\\\\\\"\");
                case '\\\\' -> out.append(\"\\\\\\\\\");
                case '\\n' -> out.append(\"\\\\n\");
                case '\\r' -> out.append(\"\\\\r\");
                case '\\t' -> out.append(\"\\\\t\");
                case '\\b' -> out.append(\"\\\\b\");
                case '\\f' -> out.append(\"\\\\f\");
                default -> {
                    if (c < 0x20) {
                        out.append(String.format(\"\\\\u%04x\", (int) c));
                    } else {
                        out.append(c);
                    }
                }
            }
        }
        out.append('\"');
    }

    // ---------------------------------------------------------------- parsing

    /** Parse a JSON string into an {@code Object} tree. */
    public static Object parse(String text) {
        Parser parser = new Parser(text);
        parser.skipWhitespace();
        Object value = parser.value();
        parser.skipWhitespace();
        if (!parser.atEnd()) {
            throw new IllegalArgumentException(\"trailing content after JSON value\");
        }
        return value;
    }

    private static final class Parser {
        private final String src;
        private int pos;

        Parser(String src) {
            this.src = src;
        }

        boolean atEnd() {
            return pos >= src.length();
        }

        void skipWhitespace() {
            while (pos < src.length()) {
                char c = src.charAt(pos);
                if (c == ' ' || c == '\\t' || c == '\\n' || c == '\\r') {
                    pos++;
                } else {
                    break;
                }
            }
        }

        Object value() {
            if (atEnd()) {
                throw err(\"unexpected end of input\");
            }
            char c = src.charAt(pos);
            return switch (c) {
                case '{' -> object();
                case '[' -> array();
                case '\"' -> string();
                case 't', 'f' -> bool();
                case 'n' -> nullLiteral();
                default -> number();
            };
        }

        Map<String, Object> object() {
            expect('{');
            Map<String, Object> map = new LinkedHashMap<>();
            skipWhitespace();
            if (peek() == '}') {
                pos++;
                return map;
            }
            while (true) {
                skipWhitespace();
                String key = string();
                skipWhitespace();
                expect(':');
                skipWhitespace();
                map.put(key, value());
                skipWhitespace();
                char c = next();
                if (c == '}') {
                    break;
                }
                if (c != ',') {
                    throw err(\"expected ',' or '}' in object\");
                }
            }
            return map;
        }

        List<Object> array() {
            expect('[');
            List<Object> list = new ArrayList<>();
            skipWhitespace();
            if (peek() == ']') {
                pos++;
                return list;
            }
            while (true) {
                skipWhitespace();
                list.add(value());
                skipWhitespace();
                char c = next();
                if (c == ']') {
                    break;
                }
                if (c != ',') {
                    throw err(\"expected ',' or ']' in array\");
                }
            }
            return list;
        }

        String string() {
            expect('\"');
            StringBuilder sb = new StringBuilder();
            while (true) {
                if (atEnd()) {
                    throw err(\"unterminated string\");
                }
                char c = src.charAt(pos++);
                if (c == '\"') {
                    break;
                }
                if (c == '\\\\') {
                    char e = next();
                    switch (e) {
                        case '\"' -> sb.append('\"');
                        case '\\\\' -> sb.append('\\\\');
                        case '/' -> sb.append('/');
                        case 'n' -> sb.append('\\n');
                        case 'r' -> sb.append('\\r');
                        case 't' -> sb.append('\\t');
                        case 'b' -> sb.append('\\b');
                        case 'f' -> sb.append('\\f');
                        case 'u' -> {
                            String hex = src.substring(pos, pos + 4);
                            sb.append((char) Integer.parseInt(hex, 16));
                            pos += 4;
                        }
                        default -> throw err(\"invalid escape\");
                    }
                } else {
                    sb.append(c);
                }
            }
            return sb.toString();
        }

        Boolean bool() {
            if (src.startsWith(\"true\", pos)) {
                pos += 4;
                return Boolean.TRUE;
            }
            if (src.startsWith(\"false\", pos)) {
                pos += 5;
                return Boolean.FALSE;
            }
            throw err(\"invalid literal\");
        }

        Object nullLiteral() {
            if (src.startsWith(\"null\", pos)) {
                pos += 4;
                return null;
            }
            throw err(\"invalid literal\");
        }

        Object number() {
            int start = pos;
            boolean floating = false;
            while (pos < src.length()) {
                char c = src.charAt(pos);
                if (c == '-' || c == '+' || (c >= '0' && c <= '9')) {
                    pos++;
                } else if (c == '.' || c == 'e' || c == 'E') {
                    floating = true;
                    pos++;
                } else {
                    break;
                }
            }
            String num = src.substring(start, pos);
            if (num.isEmpty()) {
                throw err(\"invalid number\");
            }
            if (floating) {
                return Double.valueOf(num);
            }
            try {
                return Long.valueOf(num);
            } catch (NumberFormatException ex) {
                return Double.valueOf(num);
            }
        }

        char peek() {
            return atEnd() ? '\\0' : src.charAt(pos);
        }

        char next() {
            if (atEnd()) {
                throw err(\"unexpected end of input\");
            }
            return src.charAt(pos++);
        }

        void expect(char c) {
            if (next() != c) {
                throw err(\"expected '\" + c + \"'\");
            }
        }

        IllegalArgumentException err(String message) {
            return new IllegalArgumentException(
                \"JSON parse error: \" + message + \" at offset \" + pos);
        }
    }

    // ------------------------------------- typed coercions for generated code

    /** Coerce a parsed value to a JSON object, or {@code null}. */
    @SuppressWarnings(\"unchecked\")
    public static Map<String, Object> asObject(Object value) {
        if (value == null) {
            return null;
        }
        if (value instanceof Map<?, ?> m) {
            return (Map<String, Object>) m;
        }
        throw new IllegalArgumentException(
            \"expected a JSON object, got \" + value.getClass().getName());
    }

    /** Coerce a parsed value to a JSON array, or {@code null}. */
    @SuppressWarnings(\"unchecked\")
    public static List<Object> asList(Object value) {
        if (value == null) {
            return null;
        }
        if (value instanceof List<?> l) {
            return (List<Object>) l;
        }
        throw new IllegalArgumentException(
            \"expected a JSON array, got \" + value.getClass().getName());
    }

    /** Coerce a parsed value to a string, or {@code null}. */
    public static String asString(Object value) {
        if (value == null) {
            return null;
        }
        if (value instanceof String s) {
            return s;
        }
        throw new IllegalArgumentException(
            \"expected a JSON string, got \" + value.getClass().getName());
    }

    /** Coerce a parsed value to a 64-bit integer, or {@code null}. */
    public static Long asLong(Object value) {
        if (value == null) {
            return null;
        }
        if (value instanceof Number n) {
            return n.longValue();
        }
        throw new IllegalArgumentException(
            \"expected a JSON number, got \" + value.getClass().getName());
    }

    /** Coerce a parsed value to a double, or {@code null}. */
    public static Double asDouble(Object value) {
        if (value == null) {
            return null;
        }
        if (value instanceof Number n) {
            return n.doubleValue();
        }
        throw new IllegalArgumentException(
            \"expected a JSON number, got \" + value.getClass().getName());
    }

    /** Coerce a parsed value to a boolean, or {@code null}. */
    public static Boolean asBoolean(Object value) {
        if (value == null) {
            return null;
        }
        if (value instanceof Boolean b) {
            return b;
        }
        throw new IllegalArgumentException(
            \"expected a JSON boolean, got \" + value.getClass().getName());
    }

    /** Encode a typed list to a JSON array, element by element. */
    public static <T> List<Object> encodeList(List<T> items, Function<T, Object> encoder) {
        if (items == null) {
            return null;
        }
        List<Object> out = new ArrayList<>(items.size());
        for (T item : items) {
            out.add(encoder.apply(item));
        }
        return out;
    }

    /** Encode a typed string-keyed map to a JSON object, value by value. */
    public static <T> Map<String, Object> encodeMap(Map<String, T> items, Function<T, Object> encoder) {
        if (items == null) {
            return null;
        }
        Map<String, Object> out = new LinkedHashMap<>();
        for (Map.Entry<String, T> entry : items.entrySet()) {
            out.put(entry.getKey(), encoder.apply(entry.getValue()));
        }
        return out;
    }

    /** Decode a JSON array to a typed list, element by element. */
    public static <T> List<T> decodeList(Object json, Function<Object, T> decoder) {
        if (json == null) {
            return null;
        }
        List<Object> raw = asList(json);
        List<T> out = new ArrayList<>(raw.size());
        for (Object item : raw) {
            out.add(decoder.apply(item));
        }
        return out;
    }

    /** Decode a JSON object to a typed string-keyed map, value by value. */
    public static <T> Map<String, T> decodeMap(Object json, Function<Object, T> decoder) {
        if (json == null) {
            return null;
        }
        Map<String, Object> raw = asObject(json);
        Map<String, T> out = new LinkedHashMap<>();
        for (Map.Entry<String, Object> entry : raw.entrySet()) {
            out.put(entry.getKey(), decoder.apply(entry.getValue()));
        }
        return out;
    }
}
";
