// Hand-written Box Apex runtime (TR-Apex.6) — the `BoxClient` implementation.
//
// The generated managers build a structural `BoxRequest` and call
// `client.send(request)`; this class turns that into a real Salesforce HTTP
// callout: it resolves the base-URL class, attaches the bearer token and the
// request's headers/query/body, sends it, retries transient failures, and
// maps a non-2xx response to a `BoxApiException` (the manifest error model).
//
// **Governor limits shape the retry policy.** Apex has no `sleep`, so retries
// are immediate rather than backed-off, and the whole policy is bounded by
// the per-transaction callout limit (100). Retries cover 429 and 5xx; a
// single 401 re-attempt lets a caching token provider refresh. Callers that
// need true backoff should drive retries across transactions (e.g. Queueable).
public with sharing class BoxHttpClient implements BoxClient {
    private final BoxTokenProvider tokens;
    private final Integer maxRetries;
    private final Integer timeoutMs;
    private final Map<String, String> baseUrls;
    private final Map<String, String> defaultHeaders = new Map<String, String>();

    /** Build against a token provider with sensible defaults. */
    public BoxHttpClient(BoxTokenProvider tokens) {
        this(tokens, 2, 60000);
    }

    /** Build with an explicit retry budget and per-callout timeout (ms). */
    public BoxHttpClient(BoxTokenProvider tokens, Integer maxRetries, Integer timeoutMs) {
        if (tokens == null) {
            throw new BoxApiException('A token provider is required.');
        }
        this.tokens = tokens;
        this.maxRetries = (maxRetries == null || maxRetries < 0) ? 0 : maxRetries;
        this.timeoutMs = timeoutMs;
        this.baseUrls = defaultBaseUrls();
    }

    /** Override one base-URL class (custom / sandbox deployments). */
    public BoxHttpClient overrideBaseUrl(String key, String url) {
        this.baseUrls.put(key, url.removeEnd('/'));
        return this;
    }

    /** Add a header sent with every callout from this client — e.g. a
     * tracing or User-Agent header the embedding application wants on every
     * call. A header set on an individual BoxRequest takes precedence over a
     * same-named default. */
    public BoxHttpClient withHeader(String name, String value) {
        for (String existing : new List<String>(this.defaultHeaders.keySet())) {
            if (existing.equalsIgnoreCase(name)) {
                this.defaultHeaders.remove(existing);
            }
        }
        this.defaultHeaders.put(name, value);
        return this;
    }

    public BoxResponse send(BoxRequest request) {
        String url = buildUrl(request);
        // The 401 refresh re-attempt is tracked separately from the transient
        // retry budget, so it always gets its one shot even when maxRetries is 0.
        // The loop is bounded (1 refresh + maxRetries transient + a final
        // attempt) so the method provably returns — Apex rejects `while (true)`
        // as always-returning, hence the explicit bound and trailing throw.
        Boolean refreshed = false;
        Integer transientRetries = 0;
        Integer plannedAttempts = this.maxRetries + 2;
        // Never plan more attempts than the per-transaction callout budget can
        // fund. Each attempt makes at least one callout, and a caching token
        // provider adds a mint callout (initial, and again after a 401
        // invalidate). Clamp to the remaining budget — reserving one callout for
        // that mint — so the loop can't overrun the 100-callout limit and hit an
        // uncatchable LimitException instead of a BoxApiException.
        Integer budget = Limits.getLimitCallouts() - Limits.getCallouts();
        Integer maxAttempts = Math.min(plannedAttempts, Math.max(1, budget - 1));
        for (Integer attempt = 0; attempt < maxAttempts; attempt++) {
            HttpResponse res = callout(request, url);
            Integer status = res.getStatusCode();
            // A single 401 re-attempt gives a caching token provider a chance
            // to refresh: discard the (now-rejected) token so the next callout
            // mints a fresh one. A persistent 401 falls through to the error.
            if (status == 401 && !refreshed) {
                refreshed = true;
                this.tokens.invalidate();
                continue;
            }
            if (isRetriable(status) && transientRetries < this.maxRetries) {
                transientRetries++;
                continue;
            }
            if (status < 200 || status >= 300) {
                throw BoxApiException.fromResponse(status, res.getBody());
            }
            return toResponse(res);
        }
        throw new BoxApiException('Box API request failed: retries exhausted.');
    }

    private HttpResponse callout(BoxRequest request, String url) {
        HttpRequest req = new HttpRequest();
        req.setEndpoint(url);
        req.setMethod(request.method);
        req.setTimeout(this.timeoutMs);
        req.setHeader('Authorization', 'Bearer ' + this.tokens.getAccessToken());
        if (!this.defaultHeaders.isEmpty()) {
            Set<String> requestKeysLower = new Set<String>();
            if (request.headers != null) {
                for (String key : request.headers.keySet()) {
                    requestKeysLower.add(key.toLowerCase());
                }
            }
            for (String key : this.defaultHeaders.keySet()) {
                if (!requestKeysLower.contains(key.toLowerCase())) {
                    req.setHeader(key, this.defaultHeaders.get(key));
                }
            }
        }
        if (request.headers != null) {
            for (String key : request.headers.keySet()) {
                req.setHeader(key, request.headers.get(key));
            }
        }
        if (request.multipartJsonName != null || request.multipartFilePartName != null) {
            String boundary = 'gantryBoundary' + String.valueOf(Math.abs(Crypto.getRandomLong()));
            req.setHeader('Content-Type', 'multipart/form-data; boundary=' + boundary);
            req.setBodyAsBlob(buildMultipartBody(request, boundary));
        } else if (request.binaryBody != null) {
            req.setBodyAsBlob(request.binaryBody);
        } else if (request.body != null) {
            req.setHeader('Content-Type', 'application/json');
            // Box treats an absent key and an explicit null differently. The
            // manager decides which each field is: by default nulls are
            // suppressed (an unset field stays absent), but a body routed
            // through denormalizeKeys pre-reduces to a map of only the intended
            // keys — including any explicit nulls the caller requested (D-138) —
            // and sets suppressNulls = false so those nulls reach Box.
            req.setBody(JSON.serialize(request.body, request.suppressNulls));
        }
        return new Http().send(req);
    }

    // Salesforce's HttpRequest has no native multipart support, so this
    // assembles a Box-style multipart/form-data body by hand (G-7): a JSON
    // part named request.multipartJsonName iff set, and a binary part named
    // request.multipartFilePartName — used as both the form field name and
    // the Content-Disposition filename, since the real filename isn't
    // structurally knowable — iff set.
    private static Blob buildMultipartBody(BoxRequest request, String boundary) {
        List<Blob> parts = new List<Blob>();
        if (request.multipartJsonName != null) {
            String json = JSON.serialize(request.multipartJsonValue, request.suppressNulls);
            parts.add(Blob.valueOf(
                '--' + boundary + '\r\n' +
                'Content-Disposition: form-data; name="' + safeMultipartName(request.multipartJsonName) + '"\r\n' +
                'Content-Type: application/json\r\n\r\n' + json + '\r\n'
            ));
        }
        if (request.multipartFilePartName != null) {
            String name = safeMultipartName(request.multipartFilePartName);
            parts.add(Blob.valueOf(
                '--' + boundary + '\r\n' +
                'Content-Disposition: form-data; name="' + name + '"; filename="' + name + '"\r\n' +
                'Content-Type: application/octet-stream\r\n\r\n'
            ));
            parts.add(request.multipartFile);
            parts.add(Blob.valueOf('\r\n'));
        }
        parts.add(Blob.valueOf('--' + boundary + '--\r\n'));
        return concatBlobs(parts);
    }

    // Sanitize a multipart part name so it can't break out of the quoted
    // Content-Disposition header value (CRLF injection, unescaped quote).
    private static String safeMultipartName(String value) {
        return value.replace('\r', '').replace('\n', '')
            .replace('\\', '\\\\').replace('"', '\\"');
    }

    private static final String HEX_DIGITS = '0123456789abcdef';
    private static final String BASE64_ALPHABET =
        'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';

    // Concatenate blobs byte-exactly. Apex's `+` is an arithmetic operator —
    // not overloaded for Blob, confirmed by the compiler ("Arithmetic
    // expressions must use numeric arguments") — and Apex has no byte-array
    // Blob constructor either. This round-trips through hex instead of
    // base64: a byte is always exactly 2 hex characters, so hex strings from
    // different blobs concatenate safely regardless of length, unlike
    // base64's 3-byte groups (BoxChunkedUpload.cls hits that same alignment
    // constraint when it slices, not concatenates, a Blob). The combined hex
    // is then hand-encoded to base64 to reach the one guaranteed decode path,
    // EncodingUtil.base64Decode.
    private static Blob concatBlobs(List<Blob> parts) {
        String hex = '';
        for (Blob part : parts) {
            // Lower-cased defensively: hexByte's lookup only recognizes
            // lowercase digits, and a silently wrong index (-1) would corrupt
            // bytes rather than fail loudly (NF-1).
            hex += EncodingUtil.convertToHex(part).toLowerCase();
        }
        return EncodingUtil.base64Decode(hexToBase64(hex));
    }

    // Every byte is 2 hex characters; group 3 bytes (6 hex characters) into
    // 4 base64 characters, padding a final 1- or 2-byte group per the spec.
    private static String hexToBase64(String hex) {
        String out = '';
        Integer i = 0;
        Integer total = hex.length();
        while (i < total) {
            Integer remaining = total - i;
            Integer b0 = hexByte(hex, i);
            if (remaining >= 6) {
                Integer b1 = hexByte(hex, i + 2);
                Integer b2 = hexByte(hex, i + 4);
                out += base64Char(b0 >> 2);
                out += base64Char(((b0 & 3) << 4) | (b1 >> 4));
                out += base64Char(((b1 & 15) << 2) | (b2 >> 6));
                out += base64Char(b2 & 63);
                i += 6;
            } else if (remaining == 4) {
                Integer b1 = hexByte(hex, i + 2);
                out += base64Char(b0 >> 2);
                out += base64Char(((b0 & 3) << 4) | (b1 >> 4));
                out += base64Char((b1 & 15) << 2);
                out += '=';
                i += 4;
            } else {
                out += base64Char(b0 >> 2);
                out += base64Char((b0 & 3) << 4);
                out += '==';
                i += 2;
            }
        }
        return out;
    }

    private static Integer hexByte(String hex, Integer at) {
        Integer high = HEX_DIGITS.indexOf(hex.substring(at, at + 1));
        Integer low = HEX_DIGITS.indexOf(hex.substring(at + 1, at + 2));
        return (high << 4) | low;
    }

    private static String base64Char(Integer sixBits) {
        return BASE64_ALPHABET.substring(sixBits, sixBits + 1);
    }

    private String buildUrl(BoxRequest request) {
        String base = this.baseUrls.get(request.baseUrl);
        if (base == null) {
            throw new BoxApiException('Unknown base-URL class: ' + request.baseUrl);
        }
        String url = base + request.path;
        if (request.query != null && !request.query.isEmpty()) {
            List<String> pairs = new List<String>();
            for (String key : request.query.keySet()) {
                pairs.add(
                    EncodingUtil.urlEncode(key, 'UTF-8') +
                    '=' +
                    EncodingUtil.urlEncode(request.query.get(key), 'UTF-8')
                );
            }
            url += '?' + String.join(pairs, '&');
        }
        return url;
    }

    private BoxResponse toResponse(HttpResponse res) {
        BoxResponse out = new BoxResponse();
        out.statusCode = res.getStatusCode();
        out.body = res.getBody();
        out.binaryBody = res.getBodyAsBlob();
        return out;
    }

    private static Boolean isRetriable(Integer status) {
        return status == 429 || status >= 500;
    }

    // The D-106 base-URL classes, matching the keys the generator emits.
    private static Map<String, String> defaultBaseUrls() {
        return new Map<String, String>{
            'api' => 'https://api.box.com/2.0',
            'api_root' => 'https://api.box.com',
            'upload' => 'https://upload.box.com/api/2.0',
            'upload_session' => 'https://upload.box.com/api/2.0',
            'oauth_authorize' => 'https://account.box.com/api/oauth2',
            'download' => 'https://dl.boxcloud.com/2.0'
        };
    }
}
