// Hand-written Box Apex runtime (TR-Apex.6) — shared token caching + exchange.
//
// The OAuth2 token flows (Client Credentials, JWT) differ only in the POST body
// they send to the token endpoint; the rest — caching the access token,
// refreshing a minute before expiry, invalidating on demand, and normalizing
// callout/parse failures to `BoxApiException` — is identical. This abstract base
// owns all of that; a subclass supplies just `tokenRequestBody()`.
public abstract with sharing class BoxCachingTokenProvider implements BoxTokenProvider {
    // `public` (not `protected`): Apex forbids `protected static` fields, and a
    // subclass (JWT) reads this constant. A URL constant is safe to expose.
    public static final String TOKEN_URL = 'https://api.box.com/oauth2/token';
    // Refresh this many seconds before the reported expiry, so a token never
    // lapses in flight.
    private static final Long EXPIRY_SKEW_SECONDS = 60;

    private String cachedToken;
    private Long expiresAtMillis;

    /** The form-encoded token-request body (grant + credentials/assertion). */
    protected abstract String tokenRequestBody();

    /** The callout timeout in milliseconds; a subclass may override. */
    protected virtual Integer timeoutMillis() {
        return 60000;
    }

    public String getAccessToken() {
        if (
            this.cachedToken != null &&
            this.expiresAtMillis != null &&
            System.currentTimeMillis() < this.expiresAtMillis
        ) {
            return this.cachedToken;
        }
        return this.mint();
    }

    /** Drop the cached token so the next `getAccessToken()` re-mints. */
    public void invalidate() {
        this.cachedToken = null;
        this.expiresAtMillis = null;
    }

    // Exchange the request body for a token, caching it. Every failure mode —
    // callout, non-2xx, unparseable body, missing/typed token — surfaces as a
    // `BoxApiException`, the one type callers (and the HTTP client's 401 path)
    // expect to catch.
    private String mint() {
        HttpRequest req = new HttpRequest();
        req.setEndpoint(TOKEN_URL);
        req.setMethod('POST');
        req.setTimeout(this.timeoutMillis());
        req.setHeader('Content-Type', 'application/x-www-form-urlencoded');
        req.setBody(this.tokenRequestBody());

        HttpResponse res;
        try {
            res = new Http().send(req);
        } catch (System.CalloutException e) {
            throw new BoxApiException('The token request failed: ' + e.getMessage());
        }

        Integer status = res.getStatusCode();
        if (status < 200 || status >= 300) {
            throw BoxApiException.fromResponse(status, res.getBody());
        }

        Map<String, Object> parsed;
        try {
            parsed = (Map<String, Object>) JSON.deserializeUntyped(res.getBody());
        } catch (Exception e) {
            throw new BoxApiException('The token response could not be parsed: ' + e.getMessage());
        }

        Object rawToken = parsed.get('access_token');
        if (!(rawToken instanceof String) || String.isBlank((String) rawToken)) {
            throw new BoxApiException('The token response contained no access_token.');
        }
        this.cachedToken = (String) rawToken;
        this.expiresAtMillis =
            System.currentTimeMillis() + (expiresInSeconds(parsed) - EXPIRY_SKEW_SECONDS) * 1000;
        return this.cachedToken;
    }

    // `expires_in` decodes as Integer/Long/Decimal; read it through its string
    // form, defaulting to Box's usual hour when absent or unparseable.
    private static Long expiresInSeconds(Map<String, Object> parsed) {
        Object raw = parsed.get('expires_in');
        if (raw == null) {
            return 3600;
        }
        try {
            return Long.valueOf(String.valueOf(raw));
        } catch (Exception e) {
            return 3600;
        }
    }
}
