// Hand-written Box Apex runtime (TR-Apex.6) — the Client Credentials Grant.
//
// The server-to-server OAuth2 flow for a Box app with no user in the loop: POST
// the app's client id/secret (plus the enterprise or user it acts as) to the
// token endpoint, receive a short-lived access token, cache it until it expires,
// and mint a fresh one on demand. No crypto — unlike JWT, CCG needs only the
// client secret — so it works on any org without a signing key.
//
// The provider caches the token and refreshes a minute before expiry (so a
// token never expires mid-request); `invalidate()` drops the cache, which the
// HTTP client calls on a 401 to recover a token revoked ahead of schedule.
public with sharing class BoxCcgTokenProvider implements BoxTokenProvider {
    private static final String TOKEN_URL = 'https://api.box.com/oauth2/token';
    // Refresh this many seconds before the reported expiry, to avoid sending a
    // token that lapses in flight.
    private static final Long EXPIRY_SKEW_SECONDS = 60;

    private final String clientId;
    private final String clientSecret;
    private final String subjectType;
    private final String subjectId;
    private Integer timeoutMs = 60000;

    private String cachedToken;
    private Long expiresAtMillis;

    /**
     * @param clientId     the Box app's client id
     * @param clientSecret the Box app's client secret
     * @param subjectType  `'enterprise'` or `'user'` — which subject the app acts as
     * @param subjectId    the enterprise id or user id
     */
    public BoxCcgTokenProvider(
        String clientId,
        String clientSecret,
        String subjectType,
        String subjectId
    ) {
        if (String.isBlank(clientId) || String.isBlank(clientSecret)) {
            throw new BoxApiException('A client id and client secret are required.');
        }
        if (String.isBlank(subjectType) || String.isBlank(subjectId)) {
            throw new BoxApiException('A subject type and subject id are required.');
        }
        this.clientId = clientId;
        this.clientSecret = clientSecret;
        this.subjectType = subjectType;
        this.subjectId = subjectId;
    }

    /** Override the token-request callout timeout (milliseconds). */
    public BoxCcgTokenProvider withTimeout(Integer timeoutMs) {
        if (timeoutMs != null && timeoutMs > 0) {
            this.timeoutMs = timeoutMs;
        }
        return this;
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

    private String mint() {
        HttpRequest req = new HttpRequest();
        req.setEndpoint(TOKEN_URL);
        req.setMethod('POST');
        req.setTimeout(this.timeoutMs);
        req.setHeader('Content-Type', 'application/x-www-form-urlencoded');
        req.setBody(this.formBody());

        HttpResponse res = new Http().send(req);
        Integer status = res.getStatusCode();
        if (status < 200 || status >= 300) {
            throw BoxApiException.fromResponse(status, res.getBody());
        }

        Map<String, Object> parsed = (Map<String, Object>) JSON.deserializeUntyped(res.getBody());
        String token = (String) parsed.get('access_token');
        if (String.isBlank(token)) {
            throw new BoxApiException('The token response contained no access_token.');
        }
        this.cachedToken = token;
        this.expiresAtMillis =
            System.currentTimeMillis() + (expiresInSeconds(parsed) - EXPIRY_SKEW_SECONDS) * 1000;
        return token;
    }

    private String formBody() {
        return 'grant_type=client_credentials' +
            '&client_id=' + EncodingUtil.urlEncode(this.clientId, 'UTF-8') +
            '&client_secret=' + EncodingUtil.urlEncode(this.clientSecret, 'UTF-8') +
            '&box_subject_type=' + EncodingUtil.urlEncode(this.subjectType, 'UTF-8') +
            '&box_subject_id=' + EncodingUtil.urlEncode(this.subjectId, 'UTF-8');
    }

    // `expires_in` decodes as an Integer/Long/Decimal depending on size; read
    // it through its string form so any numeric shape parses, defaulting to
    // Box's usual hour when it is absent.
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
