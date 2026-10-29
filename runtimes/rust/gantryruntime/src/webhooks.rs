//! Box webhook signature verification (Box's v2 scheme).
//!
//! Each `BOX-SIGNATURE-*` header of a delivery is the base64 HMAC-SHA256,
//! under that signature key, of the body followed by the
//! `BOX-DELIVERY-TIMESTAMP` header. HMAC is built here on the runtime's existing
//! `sha2` dependency rather than adding one.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use sha2::{Digest as _, Sha256};

/// How old a delivery may be before it's rejected as a possible replay: Box's
/// ten minutes.
pub const DEFAULT_WEBHOOK_MAX_AGE: Duration = Duration::from_secs(600);

/// Checks that a webhook delivery came from Box, unaltered and recent.
///
/// ```ignore
/// let verifier = WebhookVerifier::new().primary_key(primary).secondary_key(secondary);
/// if !verifier.verify(&body, headers.iter().map(|(k, v)| (k.as_str(), v.to_str().unwrap_or("")))) {
///     // reject: not from Box, altered, or stale
/// }
/// ```
#[derive(Clone, Debug)]
pub struct WebhookVerifier {
    primary_key: Option<String>,
    secondary_key: Option<String>,
    max_age: Duration,
}

impl Default for WebhookVerifier {
    fn default() -> Self {
        WebhookVerifier {
            primary_key: None,
            secondary_key: None,
            max_age: DEFAULT_WEBHOOK_MAX_AGE,
        }
    }
}

impl WebhookVerifier {
    /// A verifier with no keys yet and Box's ten-minute maximum age.
    pub fn new() -> Self {
        Self::default()
    }

    /// The app's primary signature key, from the Developer Console.
    pub fn primary_key(mut self, key: impl Into<String>) -> Self {
        self.primary_key = Some(key.into());
        self
    }

    /// The app's secondary signature key. A delivery is genuine when the
    /// signature of any configured key matches, so a key can be rotated while
    /// the other keeps verifying.
    pub fn secondary_key(mut self, key: impl Into<String>) -> Self {
        self.secondary_key = Some(key.into());
        self
    }

    /// How old a delivery may be (default [`DEFAULT_WEBHOOK_MAX_AGE`]).
    pub fn max_age(mut self, max_age: Duration) -> Self {
        self.max_age = max_age;
        self
    }

    /// Whether `body` and `headers` are a genuine, recent delivery from Box.
    /// Pass the body exactly as received; header names match in any case.
    pub fn verify<K: AsRef<str>, V: AsRef<str>>(
        &self,
        body: &[u8],
        headers: impl IntoIterator<Item = (K, V)>,
    ) -> bool {
        self.verify_at(body, headers, SystemTime::now())
    }

    /// [`verify`](Self::verify) against the clock reading `now`. It is false
    /// for an unknown signature version or algorithm, a delivery older than
    /// the maximum age or timestamped in the future, and a signature no
    /// configured key produces.
    pub fn verify_at<K: AsRef<str>, V: AsRef<str>>(
        &self,
        body: &[u8],
        headers: impl IntoIterator<Item = (K, V)>,
        now: SystemTime,
    ) -> bool {
        let headers: Vec<(String, String)> = headers
            .into_iter()
            .map(|(k, v)| (k.as_ref().to_ascii_lowercase(), v.as_ref().to_string()))
            .collect();
        let header = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };
        if header("box-signature-version") != Some("1")
            || header("box-signature-algorithm") != Some("HmacSHA256")
        {
            return false;
        }
        let Some(timestamp) = header("box-delivery-timestamp") else {
            return false;
        };
        let Some(sent) = unix_seconds(timestamp) else {
            return false;
        };
        // Whole seconds, as Box's own SDKs compare them.
        let at = match now.duration_since(UNIX_EPOCH) {
            Ok(elapsed) => elapsed.as_secs() as i64,
            Err(before) => -(before.duration().as_secs() as i64),
        };
        if sent > at || at - sent > self.max_age.as_secs() as i64 {
            return false;
        }

        let escaped = escape_non_ascii(body);
        let bodies = std::iter::once(body).chain(escaped.as_deref());
        let signed = [
            (&self.primary_key, "box-signature-primary"),
            (&self.secondary_key, "box-signature-secondary"),
        ];
        for (key, name) in signed {
            let (Some(key), Some(received)) = (key, header(name)) else {
                continue;
            };
            let Ok(received) = STANDARD.decode(received) else {
                continue;
            };
            for candidate in bodies.clone() {
                let expected = hmac_sha256(key.as_bytes(), &[candidate, timestamp.as_bytes()]);
                if constant_time_eq(&expected, &received) {
                    return true;
                }
            }
        }
        false
    }
}

/// HMAC-SHA256 (RFC 2104) over the concatenation of `parts`.
fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    for part in parts {
        inner.update(part);
    }
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// Equality that takes the same time wherever the inputs differ.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// `body` with every character from U+007F up written as a JSON `\uXXXX`
/// escape (UTF-16, lowercase hex) — the form Box signs. A body re-encoded as
/// raw UTF-8 on its way to the caller verifies through it. `None` when `body`
/// is already all ASCII, or is not UTF-8.
fn escape_non_ascii(body: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(body).ok()?;
    if !text.chars().any(|c| u32::from(c) >= 0x7f) {
        return None;
    }
    let mut out = String::with_capacity(text.len() + 16);
    for c in text.chars() {
        if u32::from(c) < 0x7f {
            out.push(c);
            continue;
        }
        let mut units = [0u16; 2];
        for unit in c.encode_utf16(&mut units) {
            out.push_str(&format!("\\u{unit:04x}"));
        }
    }
    Some(out.into_bytes())
}

/// The Unix time, in whole seconds, of an RFC 3339 timestamp such as
/// `2020-01-01T00:00:00-07:00` or `2020-01-01T07:00:00.123Z`.
fn unix_seconds(timestamp: &str) -> Option<i64> {
    fn number(s: Option<&str>) -> Option<i64> {
        let s = s?;
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    }
    let b = timestamp.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let field = |from: usize, to: usize| number(timestamp.get(from..to));
    let (year, month, day) = (field(0, 4)?, field(5, 7)?, field(8, 10)?);
    let (hour, minute, second) = (field(11, 13)?, field(14, 16)?, field(17, 19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut zone = timestamp.get(19..)?;
    if let Some(fraction) = zone.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        zone = &fraction[digits..];
    }
    let offset = if zone == "Z" || zone == "z" {
        0
    } else {
        let sign = match zone.as_bytes().first()? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        if zone.len() != 6 || zone.as_bytes()[3] != b':' {
            return None;
        }
        let (hours, minutes) = (number(zone.get(1..3))?, number(zone.get(4..6))?);
        if hours > 23 || minutes > 59 {
            return None;
        }
        sign * (hours * 3600 + minutes * 60)
    };
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// Days from 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    // The vectors from Box's own SDK test suite (testWebhookValidation): keys,
    // bodies, and the signatures Box computed for them, all delivered at
    // 2020-01-01T00:00:00-07:00.
    const PRIMARY_KEY: &str = "SamplePrimaryKey";
    const SECONDARY_KEY: &str = "SampleSecondaryKey";
    const TIMESTAMP: &str = "2020-01-01T00:00:00-07:00";
    const BODY: &str = r#"{"type":"webhook_event","webhook":{"id":"1234567890"},"trigger":"FILE.UPLOADED","source":{"id":"1234567890","type":"file","name":"Test.txt"}}"#;
    const PRIMARY: &str = "6TfeAW3A1PASkgboxxA5yqHNKOwFyMWuEXny/FPD5hI=";
    const SECONDARY: &str = "v+1CD1Jdo3muIcbpv5lxxgPglOqMfsNHPV899xWYydo=";
    const FILE_PREFIX: &str = r#"{"webhook":{"id":"1234567890"},"trigger":"FILE.UPLOADED","source":{"id":"1234567890","type":"file","name":""#;

    // Lowercase on purpose: lookups must not depend on the sender's casing.
    fn headers(primary: &str) -> Vec<(String, String)> {
        [
            ("box-delivery-id", "f96bb54b-ee16-4fc5-aa65-8c2d9e5b546f"),
            ("box-delivery-timestamp", TIMESTAMP),
            ("box-signature-algorithm", "HmacSHA256"),
            ("box-signature-primary", primary),
            ("BOX-SIGNATURE-SECONDARY", SECONDARY),
            ("Box-Signature-Version", "1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    fn with(name: &str, value: Option<&str>) -> Vec<(String, String)> {
        let mut h: Vec<_> = headers(PRIMARY)
            .into_iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case(name))
            .collect();
        if let Some(value) = value {
            h.push((name.to_string(), value.to_string()));
        }
        h
    }

    fn at(timestamp: &str) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(unix_seconds(timestamp).unwrap() as u64)
    }

    fn both() -> WebhookVerifier {
        WebhookVerifier::new()
            .primary_key(PRIMARY_KEY)
            .secondary_key(SECONDARY_KEY)
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        // RFC 4231 test case 2, plus a key longer than the block (case 6).
        let mac = hmac_sha256(b"Jefe", &[b"what do ya ", b"want for nothing?"]);
        assert_eq!(
            STANDARD.encode(mac),
            STANDARD.encode([
                0x5b, 0xdc, 0xc1, 0x46, 0xbf, 0x60, 0x75, 0x4e, 0x6a, 0x04, 0x24, 0x26, 0x08, 0x95,
                0x75, 0xc7, 0x5a, 0x00, 0x3f, 0x08, 0x9d, 0x27, 0x39, 0x83, 0x9d, 0xec, 0x58, 0xb9,
                0x64, 0xec, 0x38, 0x43,
            ])
        );
        let mac = hmac_sha256(
            &[0xaa; 131],
            &[b"Test Using Larger Than Block-Size Key - Hash Key First"],
        );
        assert_eq!(
            mac[..4],
            [0x60, 0xe4, 0x31, 0x59],
            "a long key is hashed first"
        );
    }

    #[test]
    fn parses_rfc_3339_timestamps() {
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_seconds(TIMESTAMP), Some(1_577_862_000));
        assert_eq!(
            unix_seconds("2020-01-01T07:00:00.250Z"),
            Some(1_577_862_000)
        );
        assert_eq!(
            unix_seconds("2024-02-29T12:30:00+05:30"),
            Some(1_709_190_000)
        );
        for bad in [
            "",
            "yesterday",
            "2020-01-01 00:00:00Z",
            "2020-13-01T00:00:00Z",
            "2020-01-01T00:00:00",
            "2020-01-01T00:00:00+0700",
            "+020-01-01T00:00:00Z",
        ] {
            assert_eq!(unix_seconds(bad), None, "{bad}");
        }
    }

    #[test]
    fn accepts_box_signatures() {
        let now = at(TIMESTAMP);
        assert!(both().verify_at(BODY.as_bytes(), headers(PRIMARY), now));
        assert!(
            WebhookVerifier::new()
                .secondary_key(SECONDARY_KEY)
                .verify_at(BODY.as_bytes(), headers("not-the-primary"), now),
            "the secondary key alone verifies, as during a primary rotation"
        );

        // Box signs non-ASCII characters as \uXXXX escapes; a body that arrives
        // as raw UTF-8 verifies through that form. Escapes already in the JSON
        // (\r, \/, \\) are signed as they are.
        for (name, signature) in [
            ("\u{30B9}\u{30AF}\u{30EA}\u{30FC}\u{30F3}\u{30B7}\u{30E7}\u{30C3}\u{30C8} 2020-08-05.txt", "LV2uCu+5NJtIHrCXDYgZ0v/PP5THGRuegw3RtdnEyuE="),
            ("\u{1F600} 2020-08-05.txt", "xF/SDZosX4le+v4A0Qn59sZhuD1RqY5KRUKzVMSbh0E="),
            (r"test \r", "SVkbKgy3dEEf2PbbzpNu2lDZS7zZ/aboU7HOZgBGrJk="),
            (r"\/", "t41PWT5ZB6OcysnD6SDy9Ud+p9hdXxIdXqcdweyZv/Q="),
            (r"\\", "ERpMZwUQsGDTfj82ehdX6VvDZfvOhK5ULNfVmwVAGe0="),
        ] {
            let body = format!("{FILE_PREFIX}{name}\"}}}}");
            assert!(
                WebhookVerifier::new()
                    .primary_key(PRIMARY_KEY)
                    .verify_at(body.as_bytes(), headers(signature), now),
                "{name}: Box's signature verifies"
            );
        }

        // Exactly the maximum age old is still in time.
        assert!(both().verify_at(
            BODY.as_bytes(),
            headers(PRIMARY),
            at("2020-01-01T07:10:00Z")
        ));
    }

    /// A delivery that must not verify: what it is, who checks it, and when.
    type Rejected<'a> = (
        &'a str,
        WebhookVerifier,
        &'a [u8],
        Vec<(String, String)>,
        SystemTime,
    );

    #[test]
    fn rejects_what_box_did_not_send() {
        let now = at(TIMESTAMP);
        let body = BODY.as_bytes();
        let tampered = BODY.replace("Test.txt", "Test.exe");
        let cases: Vec<Rejected> = vec![
            (
                "wrong keys",
                WebhookVerifier::new()
                    .primary_key("IncorrectKey")
                    .secondary_key("IncorrectKey"),
                body,
                headers(PRIMARY),
                now,
            ),
            (
                "no keys",
                WebhookVerifier::new(),
                body,
                headers(PRIMARY),
                now,
            ),
            (
                "secondary key for primary",
                WebhookVerifier::new().primary_key(SECONDARY_KEY),
                body,
                headers(PRIMARY),
                now,
            ),
            (
                "tampered body",
                both(),
                tampered.as_bytes(),
                headers(PRIMARY),
                now,
            ),
            (
                "stale",
                both(),
                body,
                headers(PRIMARY),
                at("2020-01-01T07:10:01Z"),
            ),
            (
                "future",
                both(),
                body,
                headers(PRIMARY),
                at("2020-01-01T06:59:59Z"),
            ),
            (
                "short max age",
                both().max_age(Duration::from_secs(60)),
                body,
                headers(PRIMARY),
                at("2020-01-01T07:01:01Z"),
            ),
            (
                "no timestamp",
                both(),
                body,
                with("box-delivery-timestamp", None),
                now,
            ),
            (
                "bad timestamp",
                both(),
                body,
                with("box-delivery-timestamp", Some("yesterday")),
                now,
            ),
            (
                "no version",
                both(),
                body,
                with("box-signature-version", None),
                now,
            ),
            (
                "version 2",
                both(),
                body,
                with("box-signature-version", Some("2")),
                now,
            ),
            (
                "HmacSHA1",
                both(),
                body,
                with("box-signature-algorithm", Some("HmacSHA1")),
                now,
            ),
            (
                "not base64",
                WebhookVerifier::new().primary_key(PRIMARY_KEY),
                body,
                with("box-signature-primary", Some("%%%")),
                now,
            ),
        ];
        for (name, verifier, body, headers, now) in cases {
            assert!(
                !verifier.verify_at(body, headers, now),
                "{name}: must not verify"
            );
        }
        let unsigned: Vec<_> = with("box-signature-primary", None)
            .into_iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("box-signature-secondary"))
            .collect();
        assert!(
            !both().verify_at(body, unsigned, now),
            "no signatures: must not verify"
        );
    }

    #[test]
    fn verifies_against_the_real_clock() {
        // A signature computed independently of the verifier, for a delivery
        // stamped now.
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
        // Howard Hinnant's civil_from_days, the inverse of days_from_civil.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        assert_eq!(days_from_civil(y, m, d), days);
        let timestamp = format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            rem / 3600,
            rem % 3600 / 60,
            rem % 60
        );
        let signature = STANDARD.encode(hmac_sha256(
            b"live-key",
            &[BODY.as_bytes(), timestamp.as_bytes()],
        ));
        let mut h = with("box-delivery-timestamp", Some(&timestamp));
        h.retain(|(k, _)| k != "box-signature-primary");
        h.push(("box-signature-primary".into(), signature));
        assert!(WebhookVerifier::new()
            .primary_key("live-key")
            .verify(BODY.as_bytes(), h));
    }
}
