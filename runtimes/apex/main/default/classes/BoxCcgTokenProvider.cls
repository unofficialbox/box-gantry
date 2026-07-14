// Hand-written Box Apex runtime (TR-Apex.6) — the Client Credentials Grant.
//
// The server-to-server OAuth2 flow for a Box app with no user in the loop: POST
// the app's client id/secret (plus the enterprise or user it acts as) to the
// token endpoint, receive a short-lived access token, cache it, and mint a fresh
// one on demand. No crypto — unlike JWT, CCG needs only the client secret — so
// it works on any org without a signing key.
//
// Caching, refresh-before-expiry, `invalidate()`, and error normalization live
// in `BoxCachingTokenProvider`; this class supplies only the request body.
public with sharing class BoxCcgTokenProvider extends BoxCachingTokenProvider {
    private final String clientId;
    private final String clientSecret;
    private final String subjectType;
    private final String subjectId;
    private Integer timeoutMs = 60000;

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

    protected override Integer timeoutMillis() {
        return this.timeoutMs;
    }

    protected override String tokenRequestBody() {
        return 'grant_type=client_credentials' +
            '&client_id=' + EncodingUtil.urlEncode(this.clientId, 'UTF-8') +
            '&client_secret=' + EncodingUtil.urlEncode(this.clientSecret, 'UTF-8') +
            '&box_subject_type=' + EncodingUtil.urlEncode(this.subjectType, 'UTF-8') +
            '&box_subject_id=' + EncodingUtil.urlEncode(this.subjectId, 'UTF-8');
    }
}
