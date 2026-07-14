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

    public BoxResponse send(BoxRequest request) {
        String url = buildUrl(request);
        // The 401 refresh re-attempt is tracked separately from the transient
        // retry budget, so it always gets its one shot even when maxRetries is 0.
        // The loop is bounded (1 refresh + maxRetries transient + a final
        // attempt) so the method provably returns — Apex rejects `while (true)`
        // as always-returning, hence the explicit bound and trailing throw.
        Boolean refreshed = false;
        Integer transientRetries = 0;
        Integer maxAttempts = this.maxRetries + 2;
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
        if (request.headers != null) {
            for (String key : request.headers.keySet()) {
                req.setHeader(key, request.headers.get(key));
            }
        }
        if (request.binaryBody != null) {
            req.setBodyAsBlob(request.binaryBody);
        } else if (request.body != null) {
            req.setHeader('Content-Type', 'application/json');
            // Suppress nulls: Box treats an absent key and a null value
            // differently, and the default is "leave unset".
            req.setBody(JSON.serialize(request.body, true));
        }
        return new Http().send(req);
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
