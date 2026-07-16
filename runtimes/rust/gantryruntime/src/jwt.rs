//! JWT server auth: the signing-key assertion flow (Box's `box_config.json`).
//!
//! The RSA private key is parsed (and, if encrypted, decrypted) up front so a
//! bad key fails loudly at construction rather than on the first request. Each
//! token refresh RS256-signs a short-lived, single-use JWT bearer assertion
//! that [`crate::Auth::jwt`] exchanges at Box's token endpoint.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rsa::pkcs1::DecodeRsaPrivateKey as _;
use rsa::pkcs1v15::Pkcs1v15Sign;
use rsa::pkcs8::DecodePrivateKey as _;
use rsa::RsaPrivateKey;
use sha2::{Digest as _, Sha256};

use crate::Error;

/// JWT server auth config — the fields Box's `box_config.json` carries. Set
/// exactly one subject: `enterprise_id` for the service account, or `user_id`
/// to act as a managed user.
pub struct JwtConfig {
    pub client_id: String,
    pub client_secret: String,
    /// The `publicKeyID` from the app's `box_config.json`.
    pub public_key_id: String,
    /// The RSA private key PEM (optionally passphrase-encrypted).
    pub private_key_pem: Vec<u8>,
    /// The passphrase for an encrypted `private_key_pem`, if any.
    pub passphrase: Option<String>,
    pub enterprise_id: String,
    /// Optional: act as a managed user instead of the enterprise service account.
    pub user_id: Option<String>,
    /// Optional: defaults to Box's token endpoint (custom deployments).
    pub token_url: Option<String>,
}

/// Monotonic tiebreaker so two assertions minted in the same nanosecond still
/// get distinct `jti`s (Box requires each assertion be single-use).
static JTI_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A parsed signing key plus the immutable claim inputs. Re-used across
/// refreshes; each `assertion` call mints a fresh, single-use JWT.
pub(crate) struct Signer {
    key: RsaPrivateKey,
    client_id: String,
    public_key_id: String,
    subject_type: &'static str,
    subject_id: String,
}

impl Signer {
    /// Parse (and if needed decrypt) the RSA private key up front, so a bad key
    /// is a construction error, not a first-request surprise.
    pub(crate) fn new(config: &JwtConfig) -> Result<Signer, Error> {
        let key = parse_rsa_private_key(&config.private_key_pem, config.passphrase.as_deref())?;
        let (subject_type, subject_id) = match &config.user_id {
            Some(user) => ("user", user.clone()),
            None => ("enterprise", config.enterprise_id.clone()),
        };
        Ok(Signer {
            key,
            client_id: config.client_id.clone(),
            public_key_id: config.public_key_id.clone(),
            subject_type,
            subject_id,
        })
    }

    /// Build and RS256-sign the JWT bearer assertion for `audience` (the token
    /// endpoint). The claim set is single-use: a fresh `jti` and a 45s expiry.
    pub(crate) fn assertion(&self, audience: &str) -> Result<String, Error> {
        let header = serde_json::json!({
            "alg": "RS256",
            "typ": "JWT",
            "kid": self.public_key_id,
        });
        let claims = serde_json::json!({
            "iss": self.client_id,
            "sub": self.subject_id,
            "box_sub_type": self.subject_type,
            "aud": audience,
            "jti": jti(),
            "exp": now_unix() + 45,
        });
        let signing_input = format!(
            "{}.{}",
            b64(&serde_json::to_vec(&header)?),
            b64(&serde_json::to_vec(&claims)?)
        );
        let digest = Sha256::digest(signing_input.as_bytes());
        let signature = self
            .key
            .sign(Pkcs1v15Sign::new::<Sha256>(), &digest)
            .map_err(|e| Error::new(format!("gantryruntime: signing assertion: {e}")))?;
        Ok(format!("{signing_input}.{}", b64(&signature)))
    }
}

/// Decode a PEM RSA key: encrypted PKCS#8 when a passphrase is present (Box's
/// `box_config` keys), else unencrypted PKCS#8 or PKCS#1.
fn parse_rsa_private_key(pem: &[u8], passphrase: Option<&str>) -> Result<RsaPrivateKey, Error> {
    let text = std::str::from_utf8(pem)
        .map_err(|_| Error::new("gantryruntime: private key PEM is not valid UTF-8"))?;
    if text.contains("ENCRYPTED PRIVATE KEY") {
        let passphrase = passphrase.ok_or_else(|| {
            Error::new("gantryruntime: private key is encrypted but no passphrase was given")
        })?;
        return RsaPrivateKey::from_pkcs8_encrypted_pem(text, passphrase.as_bytes())
            .map_err(|e| Error::new(format!("gantryruntime: decrypting private key: {e}")));
    }
    if let Ok(key) = RsaPrivateKey::from_pkcs8_pem(text) {
        return Ok(key);
    }
    RsaPrivateKey::from_pkcs1_pem(text)
        .map_err(|e| Error::new(format!("gantryruntime: parsing private key: {e}")))
}

/// URL-safe base64 without padding (JWS `BASE64URL`).
fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Seconds since the Unix epoch.
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A single-use assertion id: the current nanoseconds plus a monotonic counter,
/// so rapid successive assertions never collide.
fn jti() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let counter = JTI_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:032x}{counter:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::RsaPublicKey;

    /// A throwaway 2048-bit RSA key (unencrypted PKCS#8) — test-only.
    const TEST_KEY: &str = "-----BEGIN PRIVATE KEY-----\n\
MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQCpfNlamR34EPS4\n\
1LEhy+NZxgcHY4psPamsCqq+g7QLGltkhtzbeuAdq/25YLXov0OS2F3NitFIHHJP\n\
MvypSeIURjDhhJeMsmtMaXy7k4sWvzZolPQlAcmh7OVX2FqF98pnMAk5C9GxFhwn\n\
tXosRfzonbR2b5pgpJpgeSto1Mj7cc1e60HC09t/G804/BkUiN+jbjrnB8PK5fLF\n\
2LvKNSa82tDVDjaMdyoMhJOg8uF3yK+3QyRPKaGudk5Qhdy1nrjULqINW47QgBx5\n\
cuH++6QCm/+9Z92kUoHN4xGqehBJ/GAKlR6ktnVA7X0xLXDnut2Q3FuwtdqDI5IP\n\
kv0r8+nxAgMBAAECggEABHH/8pzefA5xyyE+lIALZZdIjhyOjMbUDiBpGKS0RbBH\n\
Lyx/jjzutBVTpJeRREDNlGq0kLrWp1+iB33zKFUfc8Z6UYfpQHYmubMKMJJ7mbkI\n\
XWQLjUCHpp9KGp/T+MlZffn02/hGSf2mKmWobF3ZKf65tRUgEiKiSLjhqkoeciqf\n\
qYsTYD4lMdxJxqUAVT8zypg8SdZGPG69gdx5fKXpeaFv36wR0jhuo2J0AvoP5ec9\n\
XwPzd95o8/CbmtNSOIZPBdoxVq1acfj6mkvcyf9SltSKK9PVGrC9qsvPoSSCtxvg\n\
Ph83OdU9/ADMRX53E1v6T6lKwkFNUW9T95EVtYPQewKBgQDoJSIS0fh1OHdQnWaN\n\
zJqsrUFeOoRLsV24MXk0mtplqi1JR+24KchDqrJJ5CkVACBfwH7QVpZ9drMly2zW\n\
5UdPoZInXyH3BVDxd647AhYjbkzBo8u+BnI7Pgq87eVb8DMajvtTIpdy3Iw162Kt\n\
ui3YbyOLJyf/etfshXdfwCrcSwKBgQC65296fdVVFpl112JI7KT+OZ9sYgXof36t\n\
I2F8n06kfImIKRjxnbJ6/9M95JJxul+jGs4lJMf6v2LWRkJf1jCqoSlXRNdxcJH1\n\
ZRiuolQ0rOx8OYK5zZZaFNhP8qrMJbDiLMq6HuCW04u6Wpt7Gxrlwr9KSFepvsK7\n\
lQ/DdVu1MwKBgHrbjBjhvthqtdqYMrpA2msglkPEPFfC2pKsvDS273Z2hdkOlCSv\n\
GCmXoRuyAHv4wSlrurGP4b0soMsTydpBJWhjXfIwSs1sptXkPPVFuWmu6jhg82by\n\
CmqH/y7VyFjL2n/nw+LPn89OIXY3yNWgfrrYtrriUizHWpb2W6L1FLnZAoGAQFGO\n\
mm+dL3fkfZoON5xAN0BrLWgaMmVVmY14aeOEs7QrvBCwhc1H824AKudywfJqIP4D\n\
fOLIcvDTuXtaMhLKkp19VYvaPC6J/BG7SbWRFsN/akx8QSaPnBZaTkDrJ++8jEjv\n\
xtcDYMQR7KJrqRStz+2R2KVGjaKY7uagExpa4eMCgYAvGzgy3hB/eFQMZ2XcZ6ly\n\
TKlDfGTjbpi7hP+lY4gIOIOnqhRDRoVGGmzVxScielADTFf/Im3qAkip4SvBy450\n\
0MW0UR4D54v6rrdZJShwabeU70xNNGwqDK2wXewHrD/IPsZcHAunJOd+Urv+OuqV\n\
MuHbNkWNGGKoq5Z7LO+oGg==\n\
-----END PRIVATE KEY-----\n";

    fn signer() -> Signer {
        Signer::new(&JwtConfig {
            client_id: "cid".to_string(),
            client_secret: "secret".to_string(),
            public_key_id: "kid123".to_string(),
            private_key_pem: TEST_KEY.as_bytes().to_vec(),
            passphrase: None,
            enterprise_id: "ent1".to_string(),
            user_id: None,
            token_url: None,
        })
        .unwrap()
    }

    fn decode_part(part: &str) -> serde_json::Value {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
    }

    #[test]
    fn assertion_is_a_wellformed_signed_jwt() {
        let signer = signer();
        let assertion = signer
            .assertion("https://api.box.com/oauth2/token")
            .unwrap();
        let parts: Vec<&str> = assertion.split('.').collect();
        assert_eq!(parts.len(), 3, "header.claims.signature");

        let header = decode_part(parts[0]);
        assert_eq!(header["alg"], "RS256");
        assert_eq!(header["typ"], "JWT");
        assert_eq!(header["kid"], "kid123");

        let claims = decode_part(parts[1]);
        assert_eq!(claims["iss"], "cid");
        assert_eq!(claims["sub"], "ent1");
        assert_eq!(claims["box_sub_type"], "enterprise");
        assert_eq!(claims["aud"], "https://api.box.com/oauth2/token");
        assert!(claims["jti"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(claims["exp"].as_u64().unwrap() > now_unix());

        // The signature verifies against the key's public half (RS256).
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let digest = Sha256::digest(signing_input.as_bytes());
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        RsaPublicKey::from(&signer.key)
            .verify(Pkcs1v15Sign::new::<Sha256>(), &digest, &signature)
            .expect("signature verifies");
    }

    #[test]
    fn user_subject_overrides_enterprise() {
        let signer = Signer::new(&JwtConfig {
            client_id: "cid".to_string(),
            client_secret: "secret".to_string(),
            public_key_id: "kid".to_string(),
            private_key_pem: TEST_KEY.as_bytes().to_vec(),
            passphrase: None,
            enterprise_id: "ent1".to_string(),
            user_id: Some("user9".to_string()),
            token_url: None,
        })
        .unwrap();
        let claims = decode_part(signer.assertion("aud").unwrap().split('.').nth(1).unwrap());
        assert_eq!(claims["sub"], "user9");
        assert_eq!(claims["box_sub_type"], "user");
    }

    #[test]
    fn a_bad_key_fails_at_construction() {
        let err = parse_rsa_private_key(b"not a pem", None).unwrap_err();
        assert!(err.to_string().contains("parsing private key"));
    }

    #[test]
    fn an_encrypted_key_without_passphrase_is_rejected() {
        let encrypted =
            b"-----BEGIN ENCRYPTED PRIVATE KEY-----\nMIIB\n-----END ENCRYPTED PRIVATE KEY-----\n";
        let err = parse_rsa_private_key(encrypted, None).unwrap_err();
        assert!(err.to_string().contains("no passphrase"));
    }

    /// A throwaway 2048-bit RSA key, PKCS#8 encrypted (PBES2 / AES-256-CBC,
    /// passphrase `testpass`) — the shape Box's `box_config.json` ships.
    const ENCRYPTED_TEST_KEY: &str = "-----BEGIN ENCRYPTED PRIVATE KEY-----\n\
MIIFLTBXBgkqhkiG9w0BBQ0wSjApBgkqhkiG9w0BBQwwHAQI2G9Ry9IIysgCAggA\n\
MAwGCCqGSIb3DQIJBQAwHQYJYIZIAWUDBAEqBBBzQTdkEZ8qrgjgKYwvaq5MBIIE\n\
0FziZo9wmmhZdB9BqYjAGwQ7/nWCkfoJZtRQ2Ku2OOTA8fTEwB0DTXH76zPX8BOj\n\
JIf96RvDe7628c6qsfGpWaHg9lth9wX3vhrdVXJPRXYNRjKpAAuJVOx1cXmqZqMI\n\
cC+dtAkO9FNc3JirV22nfg5LGIfkm48qiUkoJTXb3/qjPSFiogML8JgOkld6r4A+\n\
WYysQpynqjekq7otxFnvBeJW0r6g+a83OolvETB67c4i0Y8P5Ia9aRpbO3UZ6xUq\n\
P4Jo3H68OtzkhsI+spyJSwN0jLfsNUa6dutt13aIwdpaECD9aSDanJycj1ZEcN6J\n\
EeafmjUgC4ZunGJJ0iaxUiSKfENDLu/JmVGopLZZM+06uqE6qGD59zJyFhSWOG80\n\
SsDyhhsPVCaXKCT294YylGhDrwyftmi4aLMfNXE+J7SkUq4fQgW4Fxs1f7T26LNT\n\
kByYavLkdhKHvUwvsjVekpUCxayb8Oo+/XCTIusIHrLOb1HPr6HNJeqlh957Hv4m\n\
LZtT8MMnBIL3vjTJGCWJxw9Lbo1oU0+TT7CY2tPoZB7iW/AhIaNa0hqEqMgxIURQ\n\
Dq6LtkReyZXYV2c6NWxlidtKEvNbF6SCnJRrecaXj5RPAvAkmtgucGuzh3ZoqpGl\n\
Y2y6QtmLOIDsTsBZroCOVE9ra66F6JSHngKYKKmn8cco02nzB4XTGbhWkEaPA3MI\n\
djC47MDu09Ey4Fut65/DmaiTdpewaAvpTI65KZkVhXXZy6/8MsiGxz0NO31y9iLz\n\
txoQSPGCOmG+CzQOo3socJorqWDi8jcB0/P/jBvkr7GfDsah+4O7AwwjK9bz6XdZ\n\
zP6a/fG6DVBjOl8QN0+jtj63uv1qFJ/2vvPLyBehrnkehFk2rNkICZwQC4CbYDsG\n\
7dkOke3pLFycIHbK0cbsJROg7tCR5RooNQW6IIYFQwZJYDxenos4lvoMl9cHx79h\n\
6/sSK4DwbGXod8ngxUlDlvoOGa+pMu2OVJCtxBkSf+TpmSoUG9Y+1G9pm6ekDEhV\n\
KyjrLxU5hKBkVmMGrm0UZbi7tqnq7+g6VJ9uX3wuNCwaGRL/NiofPWzxP7ZPy4s/\n\
ddjdWvhWRLvCI+JYv1WCYYsydImVqwGEC6aLMuQ0oww54G42STTcouM7OXmLRrsP\n\
/Lzi83J+ziqDFVrcXqds/0j2F2F2YqrOp/C549Af7lf6DV3KLK3wR9nDrgNfurPf\n\
+eph/U3lVVM3FZwMZ0ojNSAiK/YUP0QTFQB5djXwC9ViYHA2qpnbzOTMFBjc3Nai\n\
nj+4WTwwRfiR1rv/x9FmaDsMjHtoZwnS9ntr97V6sRemGGt1G4fyWleep/dK3sq/\n\
1hpG5dSOitPjPc+PvH+/bOFcrK/duQccmCTRxHI3am2O5iPAYxbfluoCQH327vg/\n\
qVBJXseNwxF9tKScZP6ifVlWeMZ+fkky5uygzt/huoebVy2DrGLZ80lDBGrU4ghc\n\
UuU2dVCtPRhDpgZVwiIvEcLm70xD/qW0Qwt8svo+A43rgjf547ySvyaykG+Ugc4D\n\
Gksl/FVQYhZhxBrGxnHn044712MNm83iYqtSyu58XCkOfiWtbA4E1cuU34dpzXdM\n\
qmjxk2QigiX6/v0hlAXJpzibHfKKD3uUhvHy9J9G1o6d\n\
-----END ENCRYPTED PRIVATE KEY-----\n";

    #[test]
    fn an_encrypted_key_decrypts_and_signs() {
        // The advertised Box flow: an encrypted PKCS#8 key + passphrase parses,
        // and the resulting signer produces a verifiable assertion.
        let signer = Signer::new(&JwtConfig {
            client_id: "cid".to_string(),
            client_secret: "secret".to_string(),
            public_key_id: "kid".to_string(),
            private_key_pem: ENCRYPTED_TEST_KEY.as_bytes().to_vec(),
            passphrase: Some("testpass".to_string()),
            enterprise_id: "ent1".to_string(),
            user_id: None,
            token_url: None,
        })
        .expect("encrypted key decrypts");
        let assertion = signer.assertion("aud").unwrap();
        let parts: Vec<&str> = assertion.split('.').collect();
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let digest = Sha256::digest(signing_input.as_bytes());
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        RsaPublicKey::from(&signer.key)
            .verify(Pkcs1v15Sign::new::<Sha256>(), &digest, &signature)
            .expect("signature verifies");
    }

    #[test]
    fn an_encrypted_key_with_the_wrong_passphrase_fails() {
        let err = parse_rsa_private_key(ENCRYPTED_TEST_KEY.as_bytes(), Some("wrong")).unwrap_err();
        assert!(err.to_string().contains("decrypting private key"));
    }

    #[test]
    fn jtis_are_unique() {
        assert_ne!(jti(), jti());
    }
}
