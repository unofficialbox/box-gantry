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
//!   distinction, so a `Tristate<T>` wrapper (emitted into `dev.unofficialbox.core`)
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
//!   hand-written, dependency-free codec (`dev.unofficialbox.core.Json`, parse +
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
pub(crate) const ROOT_PKG: &str = "dev.unofficialbox";
/// The package the model tree hangs off (`dev.unofficialbox.model.<module>`).
pub(crate) const MODEL_PKG: &str = "dev.unofficialbox.model";
/// The package the hand-written support types live in.
pub(crate) const CORE_PKG: &str = "dev.unofficialbox.core";
/// The package the per-tag manager classes live in.
pub(crate) const MANAGERS_PKG: &str = "dev.unofficialbox.managers";
/// The package the runtime-contract surface (the stub / real runtime) lives in.
pub(crate) const RUNTIME_PKG: &str = "dev.unofficialbox.runtime";
/// The package the generation-side helper (`Internal`) lives in.
pub(crate) const INTERNAL_PKG: &str = "dev.unofficialbox.internal";
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
    // The chunked-upload orchestrator (D-183): a fixed hand-written helper over
    // the generated `ChunkedUploadsManager` that uploads a large file's parts in
    // parallel with structured concurrency (JEP 505). It references specific
    // schema types, so it is emitted **only** when the spec carries the whole
    // chunked-upload surface (VR-6: never emit code that wouldn't compile). It is
    // the *only* preview-API class — the ship pom compiles it in a separate
    // `--enable-preview` pass, so the core SDK stays a plain JDK-26 artifact and
    // enabling the flag only unlocks this parallel path.
    if emits_chunked_upload(analysis) {
        files.push(GeneratedFile {
            path: java_path(ROOT_PKG, "BoxChunkedUpload"),
            content: CHUNKED_UPLOAD.to_string(),
        });
    }
    files.extend(generate_models(analysis, build));
    files.extend(generate_managers(analysis, build));
    files.extend(generate_docs(analysis));
    files.extend(generate_tests(analysis));
    files.extend(generate_ship(build));
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// Whether the program carries the whole chunked-upload surface the
/// `BoxChunkedUpload` orchestrator references (D-183) — both the concrete
/// `dev.unofficialbox.model.schemas.*` types **and** the manager methods it calls, so
/// emitting it against a spec that lacks any of them would not compile. Emit only
/// when all are present (VR-6: never emit code that can't compile), which also
/// keeps synthetic-spec gates (round-trip, unit tests) free of the preview
/// dependency. The exact method signatures / parameter order can't be checked
/// here, so the `--enable-preview -Werror` chunked gate is the ultimate compile-
/// time backstop — a mismatch fails the build, it never ships broken code.
fn emits_chunked_upload(analysis: &gantry_sema::Analysis<'_>) -> bool {
    let program = analysis.program;
    let packages = models::package_names(program);
    let names = models::type_names(program);
    let mut fqns: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (i, decl) in program.decls.iter().enumerate() {
        let id = gantry_ir::DeclId(i as u32);
        if let (Some(pkg), Some(name)) = (packages.get(&decl.module), names.get(&id)) {
            fqns.insert(format!("{pkg}.{name}"));
        }
    }
    const REQUIRED_TYPES: [&str; 7] = [
        "schemas.UploadSession",
        "schemas.UploadPart",
        "schemas.UploadedPart",
        "schemas.Files",
        "schemas.PostFilesUploadSessionsBody",
        "schemas.PostFilesIdUploadSessionsBody",
        "schemas.PostFilesUploadSessionsIdCommitBody",
    ];
    if !REQUIRED_TYPES.iter().all(|r| fqns.contains(*r)) {
        return false;
    }
    // The four `ChunkedUploadsManager` methods the orchestrator calls, named the
    // way the manager printer names them (`managers::method_name`).
    let base_version = program
        .operations
        .first()
        .and_then(|op| op.api_version.as_ref());
    let methods: std::collections::HashSet<String> = program
        .operations
        .iter()
        .map(|op| managers::method_name(op, base_version))
        .collect();
    const REQUIRED_METHODS: [&str; 4] = [
        "createFilesUploadSessions",
        "createFilesByIdUploadSessions",
        "updateFilesUploadSessionsById",
        "commitFilesUploadSessionsById",
    ];
    REQUIRED_METHODS.iter().all(|m| methods.contains(*m))
}

/// The repo-relative path of a `.java` file for `type` in `package`
/// (`dev.unofficialbox.core` + `Tristate` → `src/main/java/dev/unofficialbox/core/Tristate.java`).
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
package dev.unofficialbox.core;

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
package dev.unofficialbox.core;

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

/// The chunked-upload orchestrator (D-183), emitted verbatim. Uses structured
/// concurrency (JEP 505, preview) to upload a large file's parts in parallel,
/// over the generated `ChunkedUploadsManager`. It's the only preview-API class:
/// compiled in its own `--enable-preview` pass so the core SDK runs on a plain
/// JDK 26 and the flag only unlocks this parallel path (Java 26).
const CHUNKED_UPLOAD: &str = r#"// Code generated by box-gantry. DO NOT EDIT.
package dev.unofficialbox;

import module java.base;
import dev.unofficialbox.model.schemas.Files;
import dev.unofficialbox.model.schemas.PostFilesIdUploadSessionsBody;
import dev.unofficialbox.model.schemas.PostFilesUploadSessionsBody;
import dev.unofficialbox.model.schemas.PostFilesUploadSessionsIdCommitBody;
import dev.unofficialbox.model.schemas.UploadPart;
import dev.unofficialbox.model.schemas.UploadSession;
import dev.unofficialbox.model.schemas.UploadedPart;

/**
 * Chunked upload orchestrator (Box's three-step protocol): create an upload
 * session, slice the content, PUT every part with its byte range + per-part
 * SHA-1 digest, then commit with the whole-file digest and the ordered part
 * list. The parts upload <b>in parallel</b> via structured concurrency (JEP 505):
 * one subtask per part, joined so the first failure cancels the rest. Wraps the
 * error-prone glue a caller shouldn't hand-write (cf. the Apex D-136 orchestrator).
 */
// StructuredTaskScope is a preview API; suppress the mandatory preview warning so
// the SDK still compiles under the -Xlint:all -Werror gate (with --enable-preview).
@SuppressWarnings("preview")
public final class BoxChunkedUpload {
    /** Max part uploads in flight at once, bounding peak buffer memory. */
    private static final int MAX_CONCURRENT_PARTS = 4;

    private final Client client;

    /** Orchestrate over an existing client's session. */
    public BoxChunkedUpload(Client client) {
        this.client = client;
    }

    /** Upload {@code content} as a new file named {@code fileName} into {@code folderId}. */
    public Files upload(byte[] content, String fileName, String folderId) {
        UploadSession session = client.chunkedUploads.createFilesUploadSessions(
                new PostFilesUploadSessionsBody(folderId, (long) content.length, fileName));
        return finish(session, content);
    }

    /** Upload {@code content} as a new version of {@code fileId}. */
    public Files uploadVersion(byte[] content, String fileName, String fileId) {
        UploadSession session = client.chunkedUploads.createFilesByIdUploadSessions(
                fileId, new PostFilesIdUploadSessionsBody((long) content.length, Optional.of(fileName)));
        return finish(session, content);
    }

    private Files finish(UploadSession session, byte[] content) {
        String id = session.id().orElseThrow(() -> fail("session returned no id"));
        long partSize = session.partSize().orElseThrow(() -> fail("session returned no part_size"));
        if (partSize <= 0) {
            throw fail("session part_size was not positive: " + partSize);
        }
        // The content is a byte[] (<= 2 GiB), and each part is <= the file, so the
        // step fits an int once bounded by content.length — narrow *after* the min
        // so a huge part_size can't overflow to a negative step.
        int step = (int) Math.min(partSize, (long) content.length);
        // Cap in-flight part uploads so a many-part file doesn't buffer every part
        // (its slice + the runtime's copy) at once — bounds peak memory.
        Semaphore window = new Semaphore(MAX_CONCURRENT_PARTS);
        List<UploadPart> parts;
        try (var scope = StructuredTaskScope.open(
                StructuredTaskScope.Joiner.<UploadPart>awaitAllSuccessfulOrThrow())) {
            List<StructuredTaskScope.Subtask<UploadPart>> subtasks = new ArrayList<>();
            for (int offset = 0; offset < content.length; offset += step) {
                int start = offset;
                int len = Math.min(step, content.length - offset);
                subtasks.add(scope.fork(() -> {
                    window.acquire();
                    try {
                        return uploadPart(id, content, start, len);
                    } finally {
                        window.release();
                    }
                }));
            }
            scope.join();
            // Read results in fork order so the committed part list is ordered.
            parts = new ArrayList<>();
            for (var subtask : subtasks) {
                parts.add(subtask.get());
            }
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new dev.unofficialbox.runtime.Runtime.BoxApiException("chunked upload interrupted: " + e);
        }
        String digest = "sha=" + sha1(content, 0, content.length);
        return client.chunkedUploads.commitFilesUploadSessionsById(
                id, digest, new PostFilesUploadSessionsIdCommitBody(parts), null);
    }

    private UploadPart uploadPart(String id, byte[] content, int start, int len) {
        byte[] part = Arrays.copyOfRange(content, start, start + len);
        String digest = "sha=" + sha1(part, 0, part.length);
        String range = "bytes " + start + "-" + (start + len - 1) + "/" + content.length;
        UploadedPart uploaded = client.chunkedUploads.updateFilesUploadSessionsById(id, digest, range, part);
        return uploaded.part().orElseThrow(() -> fail("part upload returned no part"));
    }

    private static String sha1(byte[] data, int offset, int len) {
        try {
            MessageDigest md = MessageDigest.getInstance("SHA-1");
            md.update(data, offset, len);
            return Base64.getEncoder().encodeToString(md.digest());
        } catch (NoSuchAlgorithmException e) {
            throw new dev.unofficialbox.runtime.Runtime.BoxApiException("SHA-1 is unavailable: " + e);
        }
    }

    private static dev.unofficialbox.runtime.Runtime.BoxApiException fail(String message) {
        return new dev.unofficialbox.runtime.Runtime.BoxApiException("chunked upload: " + message);
    }
}
"#;
