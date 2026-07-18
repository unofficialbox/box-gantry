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
package com.box.sdk.runtime;

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

    /** A runtime error: a failed request, auth acquisition, or body decode. */
    public static final class BoxApiException extends RuntimeException {
        private static final long serialVersionUID = 1L;

        public BoxApiException(String message) {
            super(message);
        }

        public BoxApiException(String message, Throwable cause) {
            super(message, cause);
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

        /** Build a runtime session for an authentication flow. */
        public Session(Auth auth) {
            this.auth = auth;
            this.http = HttpClient.newBuilder().connectTimeout(API_TIMEOUT).build();
            this.maxRetries = DEFAULT_MAX_RETRIES;
            this.baseUrls = defaultBaseUrls();
        }

        /** The configured base URL for a D-106 class, without a trailing slash. */
        public String baseUrl(String name) {
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
                if (response.status == 401 && !refreshed) {
                    refreshed = true;
                    token = auth.forceRefresh(token);
                    continue;
                }
                if (shouldRetry(response.status, idempotent) && attempt < maxRetries) {
                    sleep(retryDelay(response, attempt));
                    continue;
                }
                return response;
            }
            throw new BoxApiException("gantryruntime: retries exhausted");
        }

        private Response send(Request request, String token) throws IOException, InterruptedException {
            HttpRequest.Builder builder = HttpRequest.newBuilder(URI.create(buildUrl(request)))
                    .timeout(API_TIMEOUT);
            HttpRequest.BodyPublisher body = request.body == null
                    ? HttpRequest.BodyPublishers.noBody()
                    : HttpRequest.BodyPublishers.ofByteArray(request.body);
            builder.method(request.method, body);
            if (request.contentType != null) {
                builder.header("Content-Type", request.contentType);
            }
            builder.header("Authorization", "Bearer " + token);
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

    /** Return the request with a Box-style multipart body: an attributes JSON part + a file part (G-7). */
    public static Request withMultipartBody(Request request, byte[] attributes, String fileName, Stream file) {
        String boundary = "gantryBoundary" + Long.toHexString(ThreadLocalRandom.current().nextLong());
        request.body = multipartBody(boundary, attributes, fileName, file.toBytes());
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

    // ----------------------------------------------- token cache (single-flight)

    private record TokenResult(String token, long ttlSeconds) {}

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
            HttpClient.newBuilder().connectTimeout(TOKEN_TIMEOUT).build();

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
        return new TokenResult(accessToken, ttl);
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

    private static byte[] multipartBody(String boundary, byte[] attributes, String fileName, byte[] file) {
        // Sanitize the filename so it can't break out of the header field.
        String safeName = fileName.replace("\r", "").replace("\n", "")
                .replace("\\", "\\\\").replace("\"", "\\\"");
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        try {
            write(out, "--" + boundary + "\r\n");
            write(out, "Content-Disposition: form-data; name=\"attributes\"\r\n");
            write(out, "Content-Type: application/json\r\n\r\n");
            out.write(attributes);
            write(out, "\r\n--" + boundary + "\r\n");
            write(out, "Content-Disposition: form-data; name=\"file\"; filename=\"" + safeName + "\"\r\n");
            write(out, "Content-Type: application/octet-stream\r\n\r\n");
            out.write(file);
            write(out, "\r\n--" + boundary + "--\r\n");
        } catch (IOException err) {
            throw new BoxApiException("gantryruntime: failed to assemble multipart body", err);
        }
        return out.toByteArray();
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
