// Hand-written Box Apex runtime (TR-Apex.6) — the JWT (server auth) flow.
//
// Box's JWT flow authenticates an app with no user in the loop by proving
// possession of an RSA private key: build a short-lived JWT assertion, sign it
// RS256, and exchange it at the token endpoint for an access token.
//
// The private key never touches Apex or source. The developer imports it into
// Salesforce **Certificate and Key Management** and registers the matching
// public key with Box (yielding a public-key id); this provider signs through
// that stored certificate by name (`Crypto.signWithCertificate`) and tags the
// JWT header with the public-key id so Box picks the right key. Like CCG, the
// access token is cached and refreshed a minute before expiry; `invalidate()`
// drops the cache for the client's 401 re-attempt.
public virtual with sharing class BoxJwtTokenProvider implements BoxTokenProvider {
    private static final String TOKEN_URL = 'https://api.box.com/oauth2/token';
    private static final String JWT_GRANT = 'urn:ietf:params:oauth:grant-type:jwt-bearer';
    private static final Long EXPIRY_SKEW_SECONDS = 60;
    // The assertion lifetime; Box rejects an `exp` more than 60s ahead.
    private static final Long ASSERTION_TTL_SECONDS = 45;

    private final String clientId;
    private final String clientSecret;
    private final String subjectType;
    private final String subjectId;
    private final String publicKeyId;
    private final String certDevName;
    private Integer timeoutMs = 60000;

    private String cachedToken;
    private Long expiresAtMillis;

    /**
     * @param clientId     the Box app's client id
     * @param clientSecret the Box app's client secret
     * @param subjectType  `'enterprise'` or `'user'` — which subject the app acts as
     * @param subjectId    the enterprise id or user id
     * @param publicKeyId  the id Box assigned the registered public key (the JWT `kid`)
     * @param certDevName  unique name of the RSA key in Salesforce Cert & Key Management
     */
    public BoxJwtTokenProvider(
        String clientId,
        String clientSecret,
        String subjectType,
        String subjectId,
        String publicKeyId,
        String certDevName
    ) {
        if (String.isBlank(clientId) || String.isBlank(clientSecret)) {
            throw new BoxApiException('A client id and client secret are required.');
        }
        if (String.isBlank(subjectType) || String.isBlank(subjectId)) {
            throw new BoxApiException('A subject type and subject id are required.');
        }
        if (String.isBlank(publicKeyId) || String.isBlank(certDevName)) {
            throw new BoxApiException('A public key id and a certificate name are required.');
        }
        this.clientId = clientId;
        this.clientSecret = clientSecret;
        this.subjectType = subjectType;
        this.subjectId = subjectId;
        this.publicKeyId = publicKeyId;
        this.certDevName = certDevName;
    }

    /** Override the token-request callout timeout (milliseconds). */
    public BoxJwtTokenProvider withTimeout(Integer timeoutMs) {
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
        String assertion = this.buildAssertion();
        HttpRequest req = new HttpRequest();
        req.setEndpoint(TOKEN_URL);
        req.setMethod('POST');
        req.setTimeout(this.timeoutMs);
        req.setHeader('Content-Type', 'application/x-www-form-urlencoded');
        req.setBody(
            'grant_type=' + EncodingUtil.urlEncode(JWT_GRANT, 'UTF-8') +
            '&assertion=' + EncodingUtil.urlEncode(assertion, 'UTF-8') +
            '&client_id=' + EncodingUtil.urlEncode(this.clientId, 'UTF-8') +
            '&client_secret=' + EncodingUtil.urlEncode(this.clientSecret, 'UTF-8')
        );

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

    /** The signed `header.claims.signature` assertion. */
    private String buildAssertion() {
        Long nowSeconds = System.currentTimeMillis() / 1000;
        String header = base64Url(
            Blob.valueOf(
                JSON.serialize(
                    new Map<String, Object>{
                        'alg' => 'RS256',
                        'typ' => 'JWT',
                        'kid' => this.publicKeyId
                    }
                )
            )
        );
        String claims = base64Url(
            Blob.valueOf(
                JSON.serialize(
                    new Map<String, Object>{
                        'iss' => this.clientId,
                        'sub' => this.subjectId,
                        'box_sub_type' => this.subjectType,
                        'aud' => TOKEN_URL,
                        // A random nonce, so an assertion can't be replayed.
                        'jti' => EncodingUtil.convertToHex(Crypto.generateAesKey(128)),
                        'exp' => nowSeconds + ASSERTION_TTL_SECONDS
                    }
                )
            )
        );
        String signingInput = header + '.' + claims;
        return signingInput + '.' + this.sign(signingInput);
    }

    /**
     * RS256-sign the JWT input through the org-stored certificate, returning the
     * base64url signature. A `virtual` seam so tests can substitute a fixed
     * signature (`Crypto.signWithCertificate` needs a real key in the org).
     */
    @TestVisible
    protected virtual String sign(String signingInput) {
        return base64Url(
            Crypto.signWithCertificate('RSA-SHA256', Blob.valueOf(signingInput), this.certDevName)
        );
    }

    // Base64url per the JWT spec: base64 with URL-safe glyphs and no padding.
    private static String base64Url(Blob input) {
        return EncodingUtil.base64Encode(input)
            .replace('+', '-')
            .replace('/', '_')
            .replace('=', '');
    }

    // `expires_in` decodes as Integer/Long/Decimal; read it through its string
    // form, defaulting to Box's usual hour when absent.
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
