// The hand-written Java runtime implementing the box-gantry V1 contract (FR-5).
//
// This is NOT generated — it is the real implementation the generated SDK calls
// through. Its public surface is byte-for-byte compatible with the compile-time
// stub the engine emits (crates/gantry-contract/src/java_stubs.rs), so the swap
// test can drop this file over the stub and the whole SDK compiles unchanged
// (FR-5.2). Pure JDK: java.net.http for transport, java.security-free until the
// JWT slice — no third-party dependency, so the swap stays a javac-only step.
//
// Scope (M7 runtime slice 1): the fetch retry/backoff loop, the request/response
// envelopes, and the developer-token + CCG (client-credentials) auth flows. The
// OAuth-refresh and JWT flows + the VR-7 live smoke are the next slice; the Auth
// abstraction and single-flight token cache are already shaped to accept them.
package dev.unofficialbox.runtime;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.net.URI;
import java.net.URLEncoder;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.ThreadLocalRandom;
import java.util.concurrent.locks.ReentrantLock;
import java.util.function.Supplier;

/** The hand-written runtime surface (FR-5). */
public final class Runtime {
    private Runtime() {}

    /** A token within this margin of expiry is treated as stale and refreshed. */
    private static final long REFRESH_MARGIN_MS = 60_000L;
    /** Exponential-backoff base and cap (before jitter). */
    private static final long BACKOFF_BASE_MS = 500L;
    private static final long BACKOFF_CAP_MS = 30_000L;
    /** Ceiling on the post-Retry-After delay (a two-tier cap, mirroring Rust). */
    private static final long MAX_RETRY_DELAY_MS = 300_000L;
    private static final int DEFAULT_MAX_RETRIES = 5;
    private static final String DEFAULT_TOKEN_URL = "https://api.box.com/oauth2/token";
    private static final Duration API_TIMEOUT = Duration.ofSeconds(60);
    private static final Duration TOKEN_TIMEOUT = Duration.ofSeconds(30);

    // ------------------------------------------------------------- envelopes

    /** The runtime-owned HTTP request envelope (fully buffered, so retries replay). */
    public static final class Request {
        private final String method;
        private final String url;
        private final List<String[]> headers = new ArrayList<>();
        private final List<String[]> query = new ArrayList<>();
        private byte[] body;
        private String contentType;
        private Duration timeout;

        private Request(String method, String url) {
            this.method = method;
            this.url = url;
        }
    }

    /** The runtime-owned HTTP response envelope (body read fully into memory). */
    public static final class Response {
        private final int status;
        private final List<String[]> headers;
        private final byte[] body;

        private Response(int status, List<String[]> headers, byte[] body) {
            this.status = status;
            this.headers = headers;
            this.body = body;
        }
    }

    /** A buffered streaming body (buffered so the Streaming axis stays retry-safe). */
    public static final class Stream {
        private final byte[] data;

        private Stream(byte[] data) {
            this.data = data;
        }

        /** An empty stream (e.g. an absent multipart file part). */
        public static Stream empty() {
            return new Stream(new byte[0]);
        }

        /** A stream over a copy of the given bytes. */
        public static Stream fromBytes(byte[] data) {
            return new Stream(data.clone());
        }

        /** The stream's bytes. */
        public byte[] toBytes() {
            return data.clone();
        }
    }

    /**
     * A runtime error: a failed request, auth acquisition, body decode, or a
     * non-2xx response from Box. {@link Session#fetch} throws it for a non-2xx
     * response once the retry budget and the single 401 refresh are spent, so a
     * caller never mistakes an error body for a result; that error carries the
     * {@linkplain #statusCode() status}, Box's error fields when the body is a
     * Box error object, and the raw {@linkplain #body() body}.
     */
    public static final class BoxApiException extends RuntimeException {
        private static final long serialVersionUID = 1L;

        private final int statusCode;
        private final String code;
        private final String boxMessage;
        private final String requestId;
        private final byte[] body;

        public BoxApiException(String message) {
            this(message, (Throwable) null);
        }

        public BoxApiException(String message, Throwable cause) {
            super(message, cause);
            this.statusCode = 0;
            this.code = null;
            this.boxMessage = null;
            this.requestId = null;
            this.body = null;
        }

        private BoxApiException(String message, int statusCode, String code, String boxMessage,
                String requestId, byte[] body) {
            super(message);
            this.statusCode = statusCode;
            this.code = code;
            this.boxMessage = boxMessage;
            this.requestId = requestId;
            this.body = body;
        }

        /** The error for a non-2xx response, with Box's error fields read from
         * the body when it is a Box error object — or, from the token endpoint,
         * an OAuth 2.0 error ({@code error}, {@code error_description}). */
        static BoxApiException fromResponse(Response response) {
            Map<String, Object> payload;
            try {
                payload = JsonLite.parseObject(new String(response.body, StandardCharsets.UTF_8));
            } catch (RuntimeException notABoxError) {
                payload = Map.of();
            }
            String code = payload.get("code") instanceof String v ? v
                    : payload.get("error") instanceof String v ? v : null;
            String boxMessage = payload.get("message") instanceof String v ? v
                    : payload.get("error_description") instanceof String v ? v : null;
            String requestId = payload.get("request_id") instanceof String v ? v : null;
            StringBuilder message = new StringBuilder("box: status ").append(response.status);
            if (code != null) {
                message.append(' ').append(code);
            }
            if (boxMessage != null) {
                message.append(": ").append(boxMessage);
            }
            if (requestId != null) {
                message.append(" (request_id ").append(requestId).append(')');
            }
            return new BoxApiException(message.toString(), response.status, code, boxMessage,
                    requestId, response.body.clone());
        }

        /** The HTTP status of a non-2xx response; 0 for any other failure. */
        public int statusCode() {
            return statusCode;
        }

        /** Whether this is Box's 412 Precondition Failed: an {@code If-Match}
         * write whose etag no longer matches, because the item changed since it
         * was read. Re-read the item and retry with its new etag. */
        public boolean isPreconditionFailed() {
            return statusCode == 412;
        }

        /** Box's error {@code code} (e.g. {@code not_found}), or null. */
        public String code() {
            return code;
        }

        /** Box's error {@code message}, or null. */
        public String boxMessage() {
            return boxMessage;
        }

        /** Box's {@code request_id} — quote it to Box support — or null. */
        public String requestId() {
            return requestId;
        }

        /** The raw body of a non-2xx response, or null for any other failure. */
        public byte[] body() {
            return body == null ? null : body.clone();
        }
    }

    /**
     * Checks that a webhook delivery came from Box, unaltered and recent, by
     * Box's v2 signature scheme: each {@code BOX-SIGNATURE-*} header is the
     * base64 HMAC-SHA256, under that signature key, of the body followed by the
     * {@code BOX-DELIVERY-TIMESTAMP} header.
     */
    public static final class WebhookVerifier {
        /** How old a delivery may be before it's rejected as a possible replay:
         * Box's ten minutes. */
        public static final Duration DEFAULT_MAX_AGE = Duration.ofMinutes(10);

        private final String primaryKey;
        private final String secondaryKey;
        private final Duration maxAge;

        /** The app's signature keys from the Developer Console; either may be
         * null. A delivery is genuine when the signature of any configured key
         * matches, so a key can be rotated while the other keeps verifying. */
        public WebhookVerifier(String primaryKey, String secondaryKey) {
            this(primaryKey, secondaryKey, DEFAULT_MAX_AGE);
        }

        /** As above, accepting deliveries up to {@code maxAge} old. */
        public WebhookVerifier(String primaryKey, String secondaryKey, Duration maxAge) {
            this.primaryKey = primaryKey;
            this.secondaryKey = secondaryKey;
            this.maxAge = maxAge;
        }

        /** Whether {@code body} and {@code headers} are a genuine, recent
         * delivery from Box. Pass the body exactly as received; header names
         * match in any case. */
        public boolean verify(byte[] body, Map<String, String> headers) {
            return verify(body, headers, java.time.Instant.now());
        }

        /** {@link #verify(byte[], Map)} against the clock reading {@code now}.
         * It is false for an unknown signature version or algorithm, a delivery
         * older than the maximum age or timestamped in the future, and a
         * signature no configured key produces. */
        public boolean verify(byte[] body, Map<String, String> headers, java.time.Instant now) {
            if (!"1".equals(header(headers, "box-signature-version"))
                    || !"HmacSHA256".equals(header(headers, "box-signature-algorithm"))) {
                return false;
            }
            String timestamp = header(headers, "box-delivery-timestamp");
            long sent;
            try {
                sent = java.time.OffsetDateTime.parse(timestamp).toEpochSecond();
            } catch (RuntimeException unparseable) {
                return false;
            }
            // Whole seconds, as Box's own SDKs compare them.
            long at = now.getEpochSecond();
            if (sent > at || at - sent > maxAge.toSeconds()) {
                return false;
            }

            List<byte[]> bodies = new ArrayList<>(List.of(body));
            byte[] escaped = escapeNonAscii(body);
            if (escaped != null) {
                bodies.add(escaped);
            }
            String[][] signed = {
                {primaryKey, "box-signature-primary"},
                {secondaryKey, "box-signature-secondary"},
            };
            for (String[] pair : signed) {
                String received = header(headers, pair[1]);
                if (pair[0] == null || pair[0].isEmpty() || received == null) {
                    continue;
                }
                byte[] signature;
                try {
                    signature = java.util.Base64.getDecoder().decode(received);
                } catch (IllegalArgumentException notBase64) {
                    continue;
                }
                for (byte[] candidate : bodies) {
                    if (java.security.MessageDigest.isEqual(
                            hmac(pair[0], candidate, timestamp), signature)) {
                        return true;
                    }
                }
            }
            return false;
        }

        private static String header(Map<String, String> headers, String name) {
            for (Map.Entry<String, String> entry : headers.entrySet()) {
                if (entry.getKey() != null && entry.getKey().equalsIgnoreCase(name)) {
                    return entry.getValue();
                }
            }
            return null;
        }

        private static byte[] hmac(String key, byte[] body, String timestamp) {
            try {
                javax.crypto.Mac mac = javax.crypto.Mac.getInstance("HmacSHA256");
                mac.init(new javax.crypto.spec.SecretKeySpec(
                    key.getBytes(StandardCharsets.UTF_8), "HmacSHA256"));
                mac.update(body);
                return mac.doFinal(timestamp.getBytes(StandardCharsets.UTF_8));
            } catch (java.security.GeneralSecurityException missing) {
                // Every Java platform is required to provide HmacSHA256.
                throw new IllegalStateException("HmacSHA256 is unavailable", missing);
            }
        }

        /** {@code body} with every character from U+007F up written as a JSON
         * unicode escape (UTF-16, lowercase hex) — the form Box signs. A body
         * re-encoded as raw UTF-8 on its way to the caller verifies through it.
         * Null when {@code body} is already all ASCII, or is not UTF-8. */
        private static byte[] escapeNonAscii(byte[] body) {
            String text;
            try {
                text = StandardCharsets.UTF_8.newDecoder()
                    .decode(java.nio.ByteBuffer.wrap(body)).toString();
            } catch (java.nio.charset.CharacterCodingException notUtf8) {
                return null;
            }
            StringBuilder out = new StringBuilder(text.length() + 16);
            boolean escaped = false;
            for (int i = 0; i < text.length(); i++) {
                char unit = text.charAt(i);
                if (unit < 0x7f) {
                    out.append(unit);
                } else {
                    escaped = true;
                    out.append(String.format("\\u%04x", (int) unit));
                }
            }
            return escaped ? out.toString().getBytes(StandardCharsets.UTF_8) : null;
        }
    }

    /** A source of access tokens for the configured auth flow. */
    public interface Auth {
        /** A valid access token, acquired or refreshed as needed. */
        String accessToken();

        /**
         * Re-acquire past the cache after a 401. If a concurrent caller already
         * replaced {@code stale} with a newer token, that newer token is
         * returned instead of hitting the network (single-flight).
         */
        String forceRefresh(String stale);
    }

    // --------------------------------------------------------------- session

    /**
     * The runtime session: auth, base URLs, HTTP client, retry policy.
     * Session-receiver contract functions are its methods.
     */
    public static final class Session {
        private final Auth auth;
        private final HttpClient http;
        private final int maxRetries;
        private final Map<String, String> baseUrls;
        private final Map<String, String> baseUrlOverrides;
        private final Map<String, String> defaultHeaders;

        /** Build a runtime session for an authentication flow, with the default
         * HTTP client, retry policy, and base URLs. Equivalent to
         * {@code Session.builder(auth).build()}. */
        public Session(Auth auth) {
            this(new Builder(auth));
        }

        private Session(Builder builder) {
            this.auth = builder.auth;
            this.http = builder.http != null
                    ? builder.http
                    : defaultHttpClient(builder.httpVersion != null ? builder.httpVersion : HttpClient.Version.HTTP_3);
            this.maxRetries = builder.maxRetries;
            this.baseUrls = defaultBaseUrls();
            this.baseUrlOverrides = Map.copyOf(builder.baseUrlOverrides);
            this.defaultHeaders = Map.copyOf(builder.defaultHeaders);
        }

        /**
         * Start a configurable {@link Session} build: an injected {@link
         * HttpClient}, a retry policy (0 disables retries), and per-session
         * base-URL overrides. Embedding tools that own their own transport and
         * measurement (e.g. a load-testing harness) use this instead of the
         * process-global {@code -Dbox.baseUrl.*} escape hatch, which cannot
         * differ across sessions in the same JVM.
         */
        public static Builder builder(Auth auth) {
            return new Builder(auth);
        }

        /**
         * The configured base URL for a D-106 class, without a trailing slash.
         * A per-session override set via {@link Builder#baseUrl} takes
         * precedence; otherwise a {@code -Dbox.baseUrl.<name>} system property
         * overrides the default — the hook for sovereign/on-prem Box
         * deployments and for pointing the SDK at a mock server in tests.
         */
        public String baseUrl(String name) {
            String override = baseUrlOverrides.get(name);
            if (override == null) {
                override = System.getProperty("box.baseUrl." + name);
            }
            if (override != null && !override.isBlank()) {
                // Managers append `/...`, so honor the no-trailing-slash contract
                // even when an override is set with a stray trailing slash.
                override = override.strip();
                while (override.endsWith("/")) {
                    override = override.substring(0, override.length() - 1);
                }
                return override;
            }
            String url = baseUrls.get(name);
            if (url == null) {
                throw new BoxApiException("gantryruntime: unknown base URL class: " + name);
            }
            return url;
        }

        /** Create a request envelope for the given method and fully built URL. */
        public Request newRequest(String method, String url) {
            return new Request(method, url);
        }

        /** A valid access token for the configured auth flow. */
        public String accessToken() {
            return auth.accessToken();
        }

        /**
         * Execute the request with retries: exponential backoff + jitter, a
         * single 401 token refresh, and Retry-After handling (R§1 network layer).
         */
        public Response fetch(Request request) {
            boolean idempotent = isIdempotent(request.method);
            // The token is acquired once; the only in-loop re-acquire is the
            // single force-refresh on a 401.
            String token = auth.accessToken();
            boolean refreshed = false;
            for (int attempt = 0; attempt <= maxRetries; attempt++) {
                Response response;
                try {
                    response = send(request, token);
                } catch (IOException | InterruptedException err) {
                    if (err instanceof InterruptedException) {
                        Thread.currentThread().interrupt();
                    }
                    // A transport error may have committed a write, so retry only
                    // idempotent methods (and never past the budget).
                    if (attempt == maxRetries || !idempotent) {
                        throw new BoxApiException("gantryruntime: request failed: " + err.getMessage(), err);
                    }
                    sleep(backoff(attempt));
                    continue;
                }
                if (response.status == 401 && !refreshed && attempt < maxRetries) {
                    refreshed = true;
                    token = auth.forceRefresh(token);
                    continue;
                }
                if (shouldRetry(response.status, idempotent) && attempt < maxRetries) {
                    sleep(retryDelay(response, attempt));
                    continue;
                }
                if (response.status / 100 != 2) {
                    throw BoxApiException.fromResponse(response);
                }
                return response;
            }
            throw new BoxApiException("gantryruntime: retries exhausted");
        }

        private Response send(Request request, String token) throws IOException, InterruptedException {
            HttpRequest.Builder builder = HttpRequest.newBuilder(URI.create(buildUrl(request)))
                    .timeout(request.timeout != null ? request.timeout : API_TIMEOUT);
            HttpRequest.BodyPublisher body = request.body == null
                    ? HttpRequest.BodyPublishers.noBody()
                    : HttpRequest.BodyPublishers.ofByteArray(request.body);
            builder.method(request.method, body);
            if (request.contentType != null) {
                builder.header("Content-Type", request.contentType);
            }
            builder.header("Authorization", "Bearer " + token);
            for (Map.Entry<String, String> header : defaultHeaders.entrySet()) {
                boolean overridden = request.headers.stream()
                        .anyMatch(h -> h[0].equalsIgnoreCase(header.getKey()))
                        || (request.contentType != null && header.getKey().equalsIgnoreCase("Content-Type"));
                if (!overridden) {
                    builder.header(header.getKey(), header.getValue());
                }
            }
            for (String[] header : request.headers) {
                builder.header(header[0], header[1]);
            }
            HttpResponse<byte[]> response = http.send(builder.build(), HttpResponse.BodyHandlers.ofByteArray());
            List<String[]> headers = new ArrayList<>();
            response.headers().map().forEach((name, values) -> {
                for (String value : values) {
                    headers.add(new String[] {name, value});
                }
            });
            return new Response(response.statusCode(), headers, response.body());
        }

        private static String buildUrl(Request request) {
            if (request.query.isEmpty()) {
                return request.url;
            }
            StringBuilder out = new StringBuilder(request.url);
            out.append(request.url.indexOf('?') >= 0 ? '&' : '?');
            for (int i = 0; i < request.query.size(); i++) {
                if (i > 0) {
                    out.append('&');
                }
                String[] pair = request.query.get(i);
                out.append(encode(pair[0])).append('=').append(encode(pair[1]));
            }
            return out.toString();
        }

        /**
         * The default HTTP client construction, shared by the no-arg
         * {@link Session#Session(Auth)} constructor and any {@link Builder}
         * that does not inject its own {@link HttpClient}.
         *
         * <p>Prefers HTTP/3 (JEP 517). With the default ALT_SVC discovery the
         * first request negotiates over HTTP/2 and upgrades to HTTP/3 for
         * subsequent requests once the origin advertises it via Alt-Svc (Box
         * does), transparently falling back to HTTP/2 or HTTP/1.1 when it
         * doesn't — so no request ever fails for lack of HTTP/3. {@link
         * Builder#httpVersion} picks another preference.
         *
         * <p>Follows redirects, as the Go, Rust, and TypeScript runtimes do:
         * Box answers a file download with a 302 to its download host, and
         * {@link HttpClient}'s default ({@code NEVER}) would hand back that
         * redirect's empty body as the file.
         */
        private static HttpClient defaultHttpClient(HttpClient.Version version) {
            return HttpClient.newBuilder()
                    .version(version)
                    .followRedirects(HttpClient.Redirect.NORMAL)
                    .connectTimeout(API_TIMEOUT)
                    .build();
        }

        /**
         * A configurable construction path for {@link Session}: an injected
         * {@link HttpClient}, retry policy, and per-session base-URL
         * overrides, for callers that need to own their own transport (D-142
         * follow-up — see the runtime seams issue for embedding load-testing
         * or instrumentation harnesses over the generated SDK).
         */
        public static final class Builder {
            private final Auth auth;
            private HttpClient http;
            private HttpClient.Version httpVersion;
            private int maxRetries = DEFAULT_MAX_RETRIES;
            private final Map<String, String> baseUrlOverrides = new LinkedHashMap<>();
            private final Map<String, String> defaultHeaders = new LinkedHashMap<>();

            private Builder(Auth auth) {
                this.auth = auth;
            }

            /** Inject the HTTP client used for API requests. Defaults to an
             * HTTP/3-preferring {@link HttpClient} built internally. An injected
             * client should follow redirects ({@code Redirect.NORMAL}): file
             * downloads arrive as a 302 to Box's download host. */
            public Builder httpClient(HttpClient http) {
                this.http = http;
                return this;
            }

            /** The preferred HTTP version of the built-in client (default
             * {@link HttpClient.Version#HTTP_3}, which falls back to HTTP/2 or
             * HTTP/1.1). An injected {@link #httpClient} carries its own
             * version, so setting both is an error at {@link #build()}. Token
             * requests keep their own client. */
            public Builder httpVersion(HttpClient.Version version) {
                this.httpVersion = java.util.Objects.requireNonNull(version, "version");
                return this;
            }

            /** How many times a retriable failure is retried; 0 disables
             * retries entirely (default 5). */
            public Builder maxRetries(int maxRetries) {
                if (maxRetries < 0) {
                    throw new IllegalArgumentException("gantryruntime: maxRetries must be non-negative");
                }
                this.maxRetries = maxRetries;
                return this;
            }

            /** Override a base-URL class for this session only — takes
             * precedence over the {@code -Dbox.baseUrl.<name>} system
             * property and the built-in default (see {@link Session#baseUrl}). */
            public Builder baseUrl(String name, String url) {
                baseUrlOverrides.put(name, url);
                return this;
            }

            /** Add a header sent with every request from this session — e.g. a
             * tracing or {@code User-Agent} header the embedding application
             * wants on every call. Replaces any prior default with the same
             * name (case-insensitively). A header set on an individual {@link
             * Request} (via {@link Runtime#withHeader}) takes precedence over
             * a same-named default. */
            public Builder header(String name, String value) {
                defaultHeaders.keySet().removeIf(key -> key.equalsIgnoreCase(name));
                defaultHeaders.put(name, value);
                return this;
            }

            /** Build the configured session. */
            public Session build() {
                if (http != null && httpVersion != null) {
                    throw new IllegalStateException(
                            "gantryruntime: httpVersion applies to the built-in client; "
                                    + "set the version on the injected HttpClient instead");
                }
                return new Session(this);
            }
        }
    }

    // ------------------------------------------------- free contract methods

    /** Return the request with the header set (replacing any existing, case-insensitively). */
    public static Request withHeader(Request request, String name, String value) {
        request.headers.removeIf(header -> header[0].equalsIgnoreCase(name));
        request.headers.add(new String[] {name, value});
        return request;
    }

    /** Return the request with the query parameter appended, encoded at send time. */
    public static Request withQuery(Request request, String name, String value) {
        request.query.add(new String[] {name, value});
        return request;
    }

    /** Return the request with the serialized JSON body and content type set. */
    public static Request withJsonBody(Request request, byte[] body) {
        request.body = body.clone();
        request.contentType = "application/json";
        return request;
    }

    /** Return the request with an application/x-www-form-urlencoded body. */
    public static Request withFormBody(Request request, byte[] form) {
        request.body = form.clone();
        request.contentType = "application/x-www-form-urlencoded";
        return request;
    }

    /** Return the request with a (buffered) streaming body. */
    public static Request withStreamBody(Request request, Stream body, String contentType) {
        request.body = body.toBytes();
        request.contentType = contentType;
        return request;
    }

    /**
     * Return the request with a Box-style multipart body. Writes a JSON part named
     * {@code jsonPartName} iff {@code jsonBytes} is non-empty, and a binary part named
     * {@code filePartName} — used as both the form field name and the Content-Disposition
     * filename, since the real filename isn't structurally knowable — iff {@code file} is
     * non-empty (G-7).
     */
    public static Request withMultipartBody(
            Request request, String jsonPartName, byte[] jsonBytes, String filePartName, Stream file) {
        String boundary = "gantryBoundary" + Long.toHexString(ThreadLocalRandom.current().nextLong());
        request.body = multipartBody(boundary, jsonPartName, jsonBytes, filePartName, file.toBytes());
        request.contentType = "multipart/form-data; boundary=" + boundary;
        return request;
    }

    /** Read the whole response body. */
    public static byte[] responseBytes(Response response) {
        return response.body.clone();
    }

    /** The response body as a stream, for binary downloads (FR-7.4). */
    public static Stream responseStream(Response response) {
        return Stream.fromBytes(response.body);
    }

    /** A response header value, empty when absent (case-insensitive lookup). */
    public static String responseHeader(Response response, String name) {
        for (String[] header : response.headers) {
            if (header[0].equalsIgnoreCase(name)) {
                return header[1];
            }
        }
        return "";
    }

    /** The response status code. */
    public static long statusCode(Response response) {
        return response.status;
    }

    /** Return the request with its own timeout, in place of the session's, for
     * a call meant to wait longer (a long poll). */
    public static Request withTimeout(Request request, Duration timeout) {
        request.timeout = timeout;
        return request;
    }

    // ---------------------------------------------------------- event stream

    /**
     * One of Box's real-time servers, as {@code OPTIONS /events} lists them. A
     * non-positive {@code maxRetries} or a null/zero {@code retryTimeout} takes
     * Box's documented default.
     */
    public record LongPollServer(String url, int maxRetries, Duration retryTimeout) {
        /** A server from the listing's raw fields: {@code max_retries} is a
         * string, {@code retry_timeout} seconds; either may be absent. */
        public static LongPollServer of(String url, String maxRetries, Long retryTimeoutSeconds) {
            int retries = 0;
            if (maxRetries != null) {
                try {
                    retries = Integer.parseInt(maxRetries.trim());
                } catch (NumberFormatException unparsable) {
                    // The default applies.
                }
            }
            Duration timeout = retryTimeoutSeconds == null ? null : Duration.ofSeconds(retryTimeoutSeconds);
            return new LongPollServer(url, retries, timeout);
        }
    }

    /** A page of events, and the position to wait from next (null keeps the current one). */
    public record EventPage<E>(List<E> events, String next) {}

    /**
     * What {@link EventStream} drives: the typed calls a generated events
     * manager supplies. {@code server} lists a real-time server (null when Box
     * lists none); {@code page} fetches the events after a stream position;
     * {@code id} names an event so a repeat can be dropped (null when it has none).
     */
    public interface EventSource<E> {
        LongPollServer server();

        EventPage<E> page(String position);

        String id(E event);
    }

    /**
     * Box's long-poll event stream from a position ({@code "now"} for events
     * from here on): iterating yields the events after it, each once, then
     * blocks on a real-time server until Box reports a change and yields those.
     * A {@code reconnect} answer, or exhausting the server's retries, lists a
     * fresh server. A long poll that times out or fails in transit is retried;
     * an error response from Box is thrown. {@link #close} (from any thread)
     * stops it: a blocked {@code hasNext} then returns false.
     */
    public static final class EventStream<E> implements java.util.Iterator<E>, AutoCloseable {
        private static final int DEFAULT_LONG_POLL_RETRIES = 10;
        private static final Duration DEFAULT_LONG_POLL_TIMEOUT = Duration.ofSeconds(610);
        /** Box may deliver an event twice; this many recent event ids are kept
         * to drop the repeats (the bound Box's own SDKs use). */
        private static final int MAX_SEEN_EVENTS = 1000;

        private final Session session;
        private final EventSource<E> source;
        private final java.util.ArrayDeque<E> pending = new java.util.ArrayDeque<>();
        private java.util.Set<String> seen = new java.util.HashSet<>();
        private String position;
        private LongPollServer server;
        private int retries;
        private boolean fetched;
        private volatile boolean closed;
        private volatile Thread waiter;

        public EventStream(Session session, String position, EventSource<E> source) {
            this.session = session;
            this.position = position;
            this.source = source;
        }

        @Override
        public boolean hasNext() {
            waiter = Thread.currentThread();
            try {
                while (pending.isEmpty()) {
                    if (closed || (fetched && !waitForChange())) {
                        return false;
                    }
                    fetch();
                }
                return !closed;
            } finally {
                waiter = null;
                if (closed) {
                    // Clear the interrupt close() used to wake this thread.
                    Thread.interrupted();
                }
            }
        }

        @Override
        public E next() {
            if (!hasNext()) {
                throw new java.util.NoSuchElementException("the event stream is closed");
            }
            return pending.poll();
        }

        /** Stop the stream, waking a thread blocked in {@code hasNext}. */
        @Override
        public void close() {
            closed = true;
            Thread blocked = waiter;
            if (blocked != null) {
                blocked.interrupt();
            }
        }

        /** Fetch the events after the position, queue the unseen ones, advance. */
        private void fetch() {
            EventPage<E> page = source.page(position);
            fetched = true;
            for (E event : page.events()) {
                String id = source.id(event);
                if (id == null || seen.add(id)) {
                    pending.add(event);
                }
            }
            if (seen.size() >= MAX_SEEN_EVENTS) {
                // Repeats arrive close together: keep only this page's ids.
                seen = new java.util.HashSet<>();
                for (E event : page.events()) {
                    String id = source.id(event);
                    if (id != null) {
                        seen.add(id);
                    }
                }
            }
            if (page.next() != null && !page.next().isEmpty()) {
                position = page.next();
            }
        }

        /** Long-poll until Box reports a change after the position; false once closed. */
        private boolean waitForChange() {
            while (!closed) {
                if (server == null || retries >= server.maxRetries()) {
                    LongPollServer listed = source.server();
                    if (listed == null) {
                        throw new BoxApiException("box: no real-time server to long-poll");
                    }
                    server = new LongPollServer(
                            listed.url(),
                            listed.maxRetries() > 0 ? listed.maxRetries() : DEFAULT_LONG_POLL_RETRIES,
                            listed.retryTimeout() == null || listed.retryTimeout().isZero()
                                    ? DEFAULT_LONG_POLL_TIMEOUT
                                    : listed.retryTimeout());
                    retries = 0;
                }
                retries++;
                String message;
                try {
                    message = longPoll(server);
                } catch (BoxApiException err) {
                    if (closed) {
                        return false;
                    }
                    if (err.statusCode() != 0) {
                        throw err;
                    }
                    // Timed out or failed in transit: poll again.
                    continue;
                }
                switch (message) {
                    case "new_change" -> {
                        return true;
                    }
                    case "reconnect", "max_retries" -> server = null;
                    default -> {
                        // Anything else: poll again.
                    }
                }
            }
            return false;
        }

        /** Wait on a real-time server for Box's next message about the stream
         * after the position: {@code new_change}, {@code reconnect}, or
         * {@code max_retries}. */
        private String longPoll(LongPollServer server) {
            Request request = withTimeout(
                    withQuery(session.newRequest("GET", server.url()), "stream_position", position),
                    server.retryTimeout());
            Map<String, Object> answer =
                    JsonLite.parseObject(new String(session.fetch(request).body, StandardCharsets.UTF_8));
            return answer.get("message") instanceof String message ? message : "";
        }
    }

    // ------------------------------------------------------------ auth flows

    /** A fixed developer token (a persistent 401 surfaces rather than looping). */
    public static Auth developerToken(String token) {
        return new Auth() {
            @Override
            public String accessToken() {
                return token;
            }

            @Override
            public String forceRefresh(String stale) {
                return token;
            }
        };
    }

    /** Client-credentials-grant (CCG) server auth, caching the token to expiry. */
    public static Auth clientCredentials(CcgConfig config) {
        return new CachedToken(() -> postCcgToken(config));
    }

    /** CCG configuration: client credentials + the enterprise/user subject. */
    public static final class CcgConfig {
        private final String clientId;
        private final String clientSecret;
        private final String subjectType;
        private final String subjectId;
        private String tokenUrl = DEFAULT_TOKEN_URL;

        private CcgConfig(String clientId, String clientSecret, String subjectType, String subjectId) {
            this.clientId = clientId;
            this.clientSecret = clientSecret;
            this.subjectType = subjectType;
            this.subjectId = subjectId;
        }

        /** Authenticate as a service account for an enterprise. */
        public static CcgConfig enterprise(String clientId, String clientSecret, String enterpriseId) {
            return new CcgConfig(clientId, clientSecret, "enterprise", enterpriseId);
        }

        /** Authenticate as a managed user. */
        public static CcgConfig user(String clientId, String clientSecret, String userId) {
            return new CcgConfig(clientId, clientSecret, "user", userId);
        }

        /** Override the token endpoint (custom deployments). */
        public CcgConfig tokenUrl(String url) {
            this.tokenUrl = url;
            return this;
        }
    }

    /** OAuth 2.0 authorization-code auth, resumed from a stored refresh token. */
    public static Auth oauth(OAuthConfig config, String refreshToken) {
        return new OAuthSource(config, refreshToken, null);
    }

    /**
     * OAuth 2.0 auth that persists each rotated refresh token through a store,
     * so a restart reloads the live token rather than one Box has invalidated.
     */
    public static Auth oauthWithStore(OAuthConfig config, String refreshToken, RefreshTokenStore store) {
        return new OAuthSource(config, refreshToken, store);
    }

    /** A durable sink for the rotated refresh token (Box rotates it each exchange). */
    public interface RefreshTokenStore {
        void save(String refreshToken);
    }

    /** OAuth client configuration. */
    public static final class OAuthConfig {
        private static final String AUTHORIZE_URL = "https://account.box.com/api/oauth2/authorize";
        private final String clientId;
        private final String clientSecret;
        private String tokenUrl = DEFAULT_TOKEN_URL;

        public OAuthConfig(String clientId, String clientSecret) {
            this.clientId = clientId;
            this.clientSecret = clientSecret;
        }

        /** Override the token endpoint (custom deployments). */
        public OAuthConfig tokenUrl(String url) {
            this.tokenUrl = url;
            return this;
        }

        /** The consent URL to redirect a user to (authorization-code flow). */
        public String authorizeUrl(String redirectUri, String state) {
            return AUTHORIZE_URL
                    + "?response_type=code&client_id=" + encode(clientId)
                    + "&redirect_uri=" + encode(redirectUri)
                    + "&state=" + encode(state);
        }

        /** Exchange an authorization code for auth (seeded with the returned refresh token). */
        public Auth exchangeCode(String code, String redirectUri) {
            Map<String, String> form = new LinkedHashMap<>();
            form.put("grant_type", "authorization_code");
            form.put("code", code);
            form.put("client_id", clientId);
            form.put("client_secret", clientSecret);
            form.put("redirect_uri", redirectUri);
            TokenResult result = postTokenForm(tokenUrl, form);
            if (result.refreshToken() == null) {
                throw new BoxApiException("gantryruntime: code exchange returned no refresh_token");
            }
            return new OAuthSource(this, result.refreshToken(), null);
        }
    }

    /** JWT server auth (signing-key assertions from a `box_config.json` key). */
    public static Auth jwt(JwtConfig config) {
        Signer signer = new Signer(config);
        String tokenUrl = config.tokenUrl == null ? DEFAULT_TOKEN_URL : config.tokenUrl;
        return new CachedToken(() -> postJwtToken(signer, config, tokenUrl));
    }

    /** JWT configuration — the fields Box's `box_config.json` carries. */
    public static final class JwtConfig {
        private final String clientId;
        private final String clientSecret;
        private final String publicKeyId;
        private final String privateKeyPem;
        private final String passphrase;
        private final String subjectType;
        private final String subjectId;
        private String tokenUrl;

        private JwtConfig(String clientId, String clientSecret, String publicKeyId, String privateKeyPem,
                String passphrase, String subjectType, String subjectId) {
            this.clientId = clientId;
            this.clientSecret = clientSecret;
            this.publicKeyId = publicKeyId;
            this.privateKeyPem = privateKeyPem;
            this.passphrase = passphrase;
            this.subjectType = subjectType;
            this.subjectId = subjectId;
        }

        /** Authenticate as the enterprise service account. */
        public static JwtConfig enterprise(String clientId, String clientSecret, String publicKeyId,
                String privateKeyPem, String passphrase, String enterpriseId) {
            return new JwtConfig(clientId, clientSecret, publicKeyId, privateKeyPem, passphrase,
                    "enterprise", enterpriseId);
        }

        /** Authenticate as a managed user. */
        public static JwtConfig user(String clientId, String clientSecret, String publicKeyId,
                String privateKeyPem, String passphrase, String userId) {
            return new JwtConfig(clientId, clientSecret, publicKeyId, privateKeyPem, passphrase,
                    "user", userId);
        }

        /** Override the token endpoint (custom deployments). */
        public JwtConfig tokenUrl(String url) {
            this.tokenUrl = url;
            return this;
        }

        /** Parse a Box `box_config.json` into an enterprise JWT config. */
        public static JwtConfig fromBoxConfig(String json) {
            Map<String, Object> root = JsonLite.parseObject(json);
            Map<String, Object> settings = childObject(root, "boxAppSettings");
            Map<String, Object> appAuth = childObject(settings, "appAuth");
            return enterprise(
                    string(settings, "clientID"),
                    string(settings, "clientSecret"),
                    string(appAuth, "publicKeyID"),
                    string(appAuth, "privateKey"),
                    appAuth.get("passphrase") instanceof String p ? p : null,
                    string(root, "enterpriseID"));
        }

        private static Map<String, Object> childObject(Map<String, Object> parent, String key) {
            if (parent.get(key) instanceof Map<?, ?> child) {
                Map<String, Object> out = new LinkedHashMap<>();
                child.forEach((k, v) -> out.put(String.valueOf(k), v));
                return out;
            }
            throw new BoxApiException("gantryruntime: box_config.json missing object '" + key + "'");
        }

        private static String string(Map<String, Object> parent, String key) {
            if (parent.get(key) instanceof String value) {
                return value;
            }
            throw new BoxApiException("gantryruntime: box_config.json missing string '" + key + "'");
        }
    }

    // ----------------------------------------------- token cache (single-flight)

    private record TokenResult(String token, long ttlSeconds, String refreshToken) {}

    private static final class CachedToken implements Auth {
        private final Supplier<TokenResult> refresh;
        private final ReentrantLock lock = new ReentrantLock();
        private String token = "";
        private long expiryMillis;

        CachedToken(Supplier<TokenResult> refresh) {
            this.refresh = refresh;
        }

        @Override
        public String accessToken() {
            lock.lock();
            try {
                if (!token.isEmpty() && System.currentTimeMillis() < expiryMillis - REFRESH_MARGIN_MS) {
                    return token;
                }
                return acquire();
            } finally {
                lock.unlock();
            }
        }

        @Override
        public String forceRefresh(String stale) {
            lock.lock();
            try {
                // A concurrent caller may already have replaced the stale token.
                if (!token.isEmpty() && !token.equals(stale)) {
                    return token;
                }
                return acquire();
            } finally {
                lock.unlock();
            }
        }

        private String acquire() {
            TokenResult result = refresh.get();
            token = result.token();
            expiryMillis = System.currentTimeMillis() + result.ttlSeconds() * 1000L;
            return token;
        }
    }

    private static final HttpClient TOKEN_HTTP =
            HttpClient.newBuilder()
                    .version(HttpClient.Version.HTTP_3)
                    .connectTimeout(TOKEN_TIMEOUT)
                    .build();

    private static TokenResult postCcgToken(CcgConfig config) {
        Map<String, String> form = new LinkedHashMap<>();
        form.put("grant_type", "client_credentials");
        form.put("client_id", config.clientId);
        form.put("client_secret", config.clientSecret);
        form.put("box_subject_type", config.subjectType);
        form.put("box_subject_id", config.subjectId);
        return postTokenForm(config.tokenUrl, form);
    }

    private static TokenResult postTokenForm(String tokenUrl, Map<String, String> form) {
        StringBuilder body = new StringBuilder();
        for (Map.Entry<String, String> entry : form.entrySet()) {
            if (body.length() > 0) {
                body.append('&');
            }
            body.append(encode(entry.getKey())).append('=').append(encode(entry.getValue()));
        }
        HttpRequest request = HttpRequest.newBuilder(URI.create(tokenUrl))
                .timeout(TOKEN_TIMEOUT)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .header("Accept", "application/json")
                .POST(HttpRequest.BodyPublishers.ofString(body.toString(), StandardCharsets.UTF_8))
                .build();
        HttpResponse<String> response;
        try {
            response = TOKEN_HTTP.send(request, HttpResponse.BodyHandlers.ofString(StandardCharsets.UTF_8));
        } catch (IOException | InterruptedException err) {
            if (err instanceof InterruptedException) {
                Thread.currentThread().interrupt();
            }
            throw new BoxApiException("gantryruntime: token request failed: " + err.getMessage(), err);
        }
        if (response.statusCode() / 100 != 2) {
            throw new BoxApiException(
                    "gantryruntime: token endpoint returned " + response.statusCode() + ": " + response.body().trim());
        }
        Map<String, Object> parsed = JsonLite.parseObject(response.body());
        if (!(parsed.get("access_token") instanceof String accessToken) || accessToken.isEmpty()) {
            throw new BoxApiException("gantryruntime: token endpoint returned no access_token");
        }
        long ttl = parsed.get("expires_in") instanceof Number expires ? expires.longValue() : 0L;
        String refreshToken = parsed.get("refresh_token") instanceof String rt ? rt : null;
        return new TokenResult(accessToken, ttl, refreshToken);
    }

    /**
     * The OAuth refresh-token source: caches the access token to expiry and
     * rotates the refresh token Box returns on every exchange, persisting each
     * new one through the store so a restart never replays a dead token.
     */
    private static final class OAuthSource implements Auth {
        private final OAuthConfig config;
        private final RefreshTokenStore store;
        private final ReentrantLock lock = new ReentrantLock();
        private String token = "";
        private long expiryMillis;
        private String refreshToken;
        // The seed token is unpersisted until either saved or first successfully used.
        private boolean refreshTokenPersisted;

        OAuthSource(OAuthConfig config, String refreshToken, RefreshTokenStore store) {
            this.config = config;
            this.refreshToken = refreshToken;
            this.store = store;
            this.refreshTokenPersisted = store == null;
        }

        @Override
        public String accessToken() {
            lock.lock();
            try {
                if (!token.isEmpty() && System.currentTimeMillis() < expiryMillis - REFRESH_MARGIN_MS) {
                    return token;
                }
                return exchange();
            } finally {
                lock.unlock();
            }
        }

        @Override
        public String forceRefresh(String stale) {
            lock.lock();
            try {
                if (!token.isEmpty() && !token.equals(stale)) {
                    return token;
                }
                return exchange();
            } finally {
                lock.unlock();
            }
        }

        private String exchange() {
            // Persist the current refresh token before spending it, so a crash
            // mid-exchange doesn't strand a token Box may already have rotated.
            if (!refreshTokenPersisted && store != null) {
                store.save(refreshToken);
                refreshTokenPersisted = true;
            }
            Map<String, String> form = new LinkedHashMap<>();
            form.put("grant_type", "refresh_token");
            form.put("refresh_token", refreshToken);
            form.put("client_id", config.clientId);
            form.put("client_secret", config.clientSecret);
            TokenResult result = postTokenForm(config.tokenUrl, form);
            if (result.refreshToken() != null && !result.refreshToken().equals(refreshToken)) {
                refreshToken = result.refreshToken();
                refreshTokenPersisted = false;
                if (store != null) {
                    store.save(refreshToken);
                    refreshTokenPersisted = true;
                }
            }
            token = result.token();
            expiryMillis = System.currentTimeMillis() + result.ttlSeconds() * 1000L;
            return token;
        }
    }

    /**
     * A parsed signing key plus the immutable claim inputs. Re-used across
     * refreshes; each {@code assertion} call mints a fresh, single-use JWT.
     */
    private static final class Signer {
        private final java.security.PrivateKey key;
        private final String clientId;
        private final String publicKeyId;
        private final String subjectType;
        private final String subjectId;

        Signer(JwtConfig config) {
            this.key = parseRsaPrivateKey(config.privateKeyPem, config.passphrase);
            this.clientId = config.clientId;
            this.publicKeyId = config.publicKeyId;
            this.subjectType = config.subjectType;
            this.subjectId = config.subjectId;
        }

        /** Build and RS256-sign the single-use JWT bearer assertion for the token endpoint. */
        String assertion(String audience) {
            String header = jsonObject(
                    "alg", "RS256",
                    "typ", "JWT",
                    "kid", publicKeyId);
            long exp = java.time.Instant.now().getEpochSecond() + 45;
            String claims = "{"
                    + "\"iss\":" + jsonString(clientId) + ","
                    + "\"sub\":" + jsonString(subjectId) + ","
                    + "\"box_sub_type\":" + jsonString(subjectType) + ","
                    + "\"aud\":" + jsonString(audience) + ","
                    + "\"jti\":" + jsonString(java.util.UUID.randomUUID().toString()) + ","
                    + "\"exp\":" + exp
                    + "}";
            String signingInput = b64Url(header.getBytes(StandardCharsets.UTF_8))
                    + "." + b64Url(claims.getBytes(StandardCharsets.UTF_8));
            try {
                java.security.Signature rsa = java.security.Signature.getInstance("SHA256withRSA");
                rsa.initSign(key);
                rsa.update(signingInput.getBytes(StandardCharsets.UTF_8));
                return signingInput + "." + b64Url(rsa.sign());
            } catch (java.security.GeneralSecurityException err) {
                throw new BoxApiException("gantryruntime: signing JWT assertion: " + err.getMessage(), err);
            }
        }
    }

    private static TokenResult postJwtToken(Signer signer, JwtConfig config, String tokenUrl) {
        Map<String, String> form = new LinkedHashMap<>();
        form.put("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer");
        form.put("assertion", signer.assertion(tokenUrl));
        form.put("client_id", config.clientId);
        form.put("client_secret", config.clientSecret);
        return postTokenForm(tokenUrl, form);
    }

    // Decode a PEM RSA key: encrypted PKCS#8 when a passphrase is present (Box's
    // box_config keys), else unencrypted PKCS#8. All java.security built-in.
    private static java.security.PrivateKey parseRsaPrivateKey(String pem, String passphrase) {
        try {
            if (pem.contains("ENCRYPTED PRIVATE KEY")) {
                if (passphrase == null) {
                    throw new BoxApiException(
                            "gantryruntime: private key is encrypted but no passphrase was given");
                }
                byte[] der = pemBody(pem, "ENCRYPTED PRIVATE KEY");
                javax.crypto.EncryptedPrivateKeyInfo info = new javax.crypto.EncryptedPrivateKeyInfo(der);
                javax.crypto.SecretKeyFactory factory =
                        javax.crypto.SecretKeyFactory.getInstance(info.getAlgName());
                javax.crypto.SecretKey secret =
                        factory.generateSecret(new javax.crypto.spec.PBEKeySpec(passphrase.toCharArray()));
                java.security.spec.PKCS8EncodedKeySpec spec = info.getKeySpec(secret);
                return java.security.KeyFactory.getInstance("RSA").generatePrivate(spec);
            }
            if (pem.contains("BEGIN PRIVATE KEY")) {
                byte[] der = pemBody(pem, "PRIVATE KEY");
                return java.security.KeyFactory.getInstance("RSA")
                        .generatePrivate(new java.security.spec.PKCS8EncodedKeySpec(der));
            }
            throw new BoxApiException(
                    "gantryruntime: unsupported private key PEM (expected PKCS#8, encrypted or plain)");
        } catch (java.security.GeneralSecurityException | java.io.IOException err) {
            throw new BoxApiException("gantryruntime: parsing private key: " + err.getMessage(), err);
        }
    }

    // The DER bytes inside a PEM block, with the armor and whitespace stripped.
    private static byte[] pemBody(String pem, String label) {
        String begin = "-----BEGIN " + label + "-----";
        String end = "-----END " + label + "-----";
        int from = pem.indexOf(begin);
        int to = pem.indexOf(end);
        if (from < 0 || to < 0) {
            throw new BoxApiException("gantryruntime: malformed PEM (missing " + label + " armor)");
        }
        String base64 = pem.substring(from + begin.length(), to).replaceAll("\\s", "");
        return java.util.Base64.getDecoder().decode(base64);
    }

    private static String b64Url(byte[] bytes) {
        return java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(bytes);
    }

    // A JSON object literal from alternating string key/value pairs.
    private static String jsonObject(String... pairs) {
        StringBuilder out = new StringBuilder("{");
        for (int i = 0; i < pairs.length; i += 2) {
            if (i > 0) {
                out.append(',');
            }
            out.append(jsonString(pairs[i])).append(':').append(jsonString(pairs[i + 1]));
        }
        return out.append('}').toString();
    }

    private static String jsonString(String value) {
        StringBuilder out = new StringBuilder("\"");
        for (int i = 0; i < value.length(); i++) {
            char c = value.charAt(i);
            switch (c) {
                case '"' -> out.append("\\\"");
                case '\\' -> out.append("\\\\");
                case '\n' -> out.append("\\n");
                case '\r' -> out.append("\\r");
                case '\t' -> out.append("\\t");
                default -> {
                    if (c < 0x20) {
                        out.append(String.format("\\u%04x", (int) c));
                    } else {
                        out.append(c);
                    }
                }
            }
        }
        return out.append('"').toString();
    }

    // ---------------------------------------------------------------- helpers

    private static boolean isIdempotent(String method) {
        return switch (method.toUpperCase(Locale.ROOT)) {
            case "GET", "HEAD", "PUT", "DELETE", "OPTIONS", "TRACE" -> true;
            default -> false;
        };
    }

    // A 429 was rate-limited (never processed), so retry any method; a 5xx may
    // have committed a write, so retry only idempotent methods.
    private static boolean shouldRetry(int status, boolean idempotent) {
        return status == 429 || (status >= 500 && idempotent);
    }

    private static long backoff(int attempt) {
        long base = Math.min(BACKOFF_BASE_MS << Math.min(attempt, 20), BACKOFF_CAP_MS);
        return ThreadLocalRandom.current().nextLong(base + 1);
    }

    private static long retryDelay(Response response, int attempt) {
        long delay = backoff(attempt);
        String retryAfter = responseHeader(response, "Retry-After").trim();
        if (retryAfter.matches("\\d+")) {
            delay = Math.max(delay, Long.parseLong(retryAfter) * 1000L);
        }
        return Math.min(delay, MAX_RETRY_DELAY_MS);
    }

    private static void sleep(long millis) {
        try {
            Thread.sleep(millis);
        } catch (InterruptedException err) {
            Thread.currentThread().interrupt();
            throw new BoxApiException("gantryruntime: interrupted during backoff", err);
        }
    }

    private static String encode(String value) {
        return URLEncoder.encode(value, StandardCharsets.UTF_8);
    }

    private static byte[] multipartBody(
            String boundary, String jsonPartName, byte[] jsonBytes, String filePartName, byte[] file) {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        try {
            if (jsonBytes.length > 0) {
                write(out, "--" + boundary + "\r\n");
                write(out, "Content-Disposition: form-data; name=\"" + safeHeaderValue(jsonPartName) + "\"\r\n");
                write(out, "Content-Type: application/json\r\n\r\n");
                out.write(jsonBytes);
                write(out, "\r\n");
            }
            if (file.length > 0) {
                String safeName = safeHeaderValue(filePartName);
                write(out, "--" + boundary + "\r\n");
                write(out, "Content-Disposition: form-data; name=\"" + safeName + "\"; filename=\"" + safeName
                        + "\"\r\n");
                write(out, "Content-Type: application/octet-stream\r\n\r\n");
                out.write(file);
                write(out, "\r\n");
            }
            write(out, "--" + boundary + "--\r\n");
        } catch (IOException err) {
            throw new BoxApiException("gantryruntime: failed to assemble multipart body", err);
        }
        return out.toByteArray();
    }

    /** Sanitize a header value so it can't break out of a quoted field (CRLF, quote, backslash). */
    private static String safeHeaderValue(String value) {
        return value.replace("\r", "").replace("\n", "")
                .replace("\\", "\\\\").replace("\"", "\\\"");
    }

    private static void write(ByteArrayOutputStream out, String text) throws IOException {
        out.write(text.getBytes(StandardCharsets.UTF_8));
    }

    private static Map<String, String> defaultBaseUrls() {
        Map<String, String> urls = new LinkedHashMap<>();
        urls.put("api", "https://api.box.com/2.0");
        urls.put("api_root", "https://api.box.com");
        urls.put("upload", "https://upload.box.com/api/2.0");
        urls.put("upload_session", "https://upload.box.com/api/2.0");
        urls.put("oauth_authorize", "https://account.box.com/api/oauth2");
        urls.put("download", "https://dl.boxcloud.com/2.0");
        return urls;
    }

    // ----------------------------------- minimal JSON reader (token responses)

    // The runtime can't depend on the SDK's generated JSON codec, so a tiny
    // reader parses the flat token responses. Object → Map, enough to read
    // access_token / expires_in / refresh_token.
    private static final class JsonLite {
        private final String src;
        private int pos;

        private JsonLite(String src) {
            this.src = src;
        }

        static Map<String, Object> parseObject(String text) {
            JsonLite parser = new JsonLite(text);
            parser.skipWhitespace();
            Object value = parser.value();
            if (value instanceof Map<?, ?> map) {
                Map<String, Object> out = new LinkedHashMap<>();
                map.forEach((key, val) -> out.put(String.valueOf(key), val));
                return out;
            }
            throw new BoxApiException("gantryruntime: expected a JSON object token response");
        }

        private Object value() {
            skipWhitespace();
            char c = src.charAt(pos);
            return switch (c) {
                case '{' -> object();
                case '[' -> array();
                case '"' -> string();
                case 't' -> literal("true", Boolean.TRUE);
                case 'f' -> literal("false", Boolean.FALSE);
                case 'n' -> literal("null", null);
                default -> number();
            };
        }

        private Map<String, Object> object() {
            Map<String, Object> map = new LinkedHashMap<>();
            pos++;
            skipWhitespace();
            if (src.charAt(pos) == '}') {
                pos++;
                return map;
            }
            while (true) {
                skipWhitespace();
                String key = string();
                skipWhitespace();
                pos++; // ':'
                map.put(key, value());
                skipWhitespace();
                if (src.charAt(pos++) == '}') {
                    break;
                }
            }
            return map;
        }

        private List<Object> array() {
            List<Object> list = new ArrayList<>();
            pos++;
            skipWhitespace();
            if (src.charAt(pos) == ']') {
                pos++;
                return list;
            }
            while (true) {
                list.add(value());
                skipWhitespace();
                if (src.charAt(pos++) == ']') {
                    break;
                }
            }
            return list;
        }

        private String string() {
            StringBuilder out = new StringBuilder();
            pos++; // opening quote
            while (true) {
                char c = src.charAt(pos++);
                if (c == '"') {
                    break;
                }
                if (c == '\\') {
                    char esc = src.charAt(pos++);
                    switch (esc) {
                        case 'n' -> out.append('\n');
                        case 't' -> out.append('\t');
                        case 'r' -> out.append('\r');
                        case 'b' -> out.append('\b');
                        case 'f' -> out.append('\f');
                        case 'u' -> {
                            out.append((char) Integer.parseInt(src.substring(pos, pos + 4), 16));
                            pos += 4;
                        }
                        default -> out.append(esc);
                    }
                } else {
                    out.append(c);
                }
            }
            return out.toString();
        }

        private Object literal(String text, Object result) {
            pos += text.length();
            return result;
        }

        private Object number() {
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
            if (floating) {
                return Double.valueOf(num);
            }
            try {
                return Long.valueOf(num);
            } catch (NumberFormatException ex) {
                return Double.valueOf(num);
            }
        }

        private void skipWhitespace() {
            while (pos < src.length()) {
                char c = src.charAt(pos);
                if (c == ' ' || c == '\t' || c == '\n' || c == '\r') {
                    pos++;
                } else {
                    break;
                }
            }
        }
    }
}
