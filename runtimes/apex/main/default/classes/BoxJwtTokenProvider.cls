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
// JWT header with the public-key id so Box picks the right key. Caching,
// refresh, `invalidate()`, and error normalization live in the base; this class
// supplies the signed assertion as the request body.
public virtual with sharing class BoxJwtTokenProvider extends BoxCachingTokenProvider {
    private static final String JWT_GRANT = 'urn:ietf:params:oauth:grant-type:jwt-bearer';
    // The assertion lifetime; Box rejects an `exp` more than 60s ahead.
    private static final Long ASSERTION_TTL_SECONDS = 45;

    private final String clientId;
    private final String clientSecret;
    private final String subjectType;
    private final String subjectId;
    private final String publicKeyId;
    private final String certDevName;
    private Integer timeoutMs = 60000;

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

    protected override Integer timeoutMillis() {
        return this.timeoutMs;
    }

    protected override String tokenRequestBody() {
        String assertion = this.buildAssertion();
        return 'grant_type=' + EncodingUtil.urlEncode(JWT_GRANT, 'UTF-8') +
            '&assertion=' + EncodingUtil.urlEncode(assertion, 'UTF-8') +
            '&client_id=' + EncodingUtil.urlEncode(this.clientId, 'UTF-8') +
            '&client_secret=' + EncodingUtil.urlEncode(this.clientSecret, 'UTF-8');
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
}
