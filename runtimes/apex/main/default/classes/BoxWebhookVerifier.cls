// Hand-written Box Apex runtime (TR-Apex.6) — webhook signature verification.
//
// Box's v2 scheme: each BOX-SIGNATURE-* header of a delivery is the base64
// HMAC-SHA256, under that signature key, of the body followed by the
// BOX-DELIVERY-TIMESTAMP header. In an @RestResource handler:
//
//     RestRequest req = RestContext.request;
//     BoxWebhookVerifier verifier = new BoxWebhookVerifier(primaryKey, secondaryKey);
//     if (!verifier.verify(req.requestBody, req.headers)) {
//         // reject: not from Box, altered, or stale
//     }
public class BoxWebhookVerifier {
    /** How old a delivery may be, in seconds, before it's rejected as a
     * possible replay: Box's ten minutes. */
    public static final Integer DEFAULT_MAX_AGE_SECONDS = 600;

    private static final String HEX_DIGITS = '0123456789abcdef';
    private static final Pattern RFC_3339 = Pattern.compile(
        '(\\d{4})-(\\d{2})-(\\d{2})[Tt](\\d{2}):(\\d{2}):(\\d{2})(?:\\.\\d+)?(?:([Zz])|([+-])(\\d{2}):(\\d{2}))'
    );

    private final String primaryKey;
    private final String secondaryKey;
    /** How old a delivery may be, in seconds. */
    public Integer maxAgeSeconds = DEFAULT_MAX_AGE_SECONDS;

    /** The app's signature keys from the Developer Console; either may be
     * null. A delivery is genuine when the signature of any configured key
     * matches, so a key can be rotated while the other keeps verifying. */
    public BoxWebhookVerifier(String primaryKey, String secondaryKey) {
        this.primaryKey = primaryKey;
        this.secondaryKey = secondaryKey;
    }

    /** Whether `body` and `headers` are a genuine, recent delivery from Box.
     * Pass the body exactly as received; header names match in any case. */
    public Boolean verify(Blob body, Map<String, String> headers) {
        return verify(body, headers, Datetime.now());
    }

    /** `verify` against the clock reading `now`. It is false for an unknown
     * signature version or algorithm, a delivery older than the maximum age or
     * timestamped in the future, and a signature no configured key produces. */
    public Boolean verify(Blob body, Map<String, String> headers, Datetime now) {
        if (body == null || headers == null) {
            return false;
        }
        // `equals`, not `==`: Apex's `==` ignores case, and Box's values don't.
        if (!'1'.equals(header(headers, 'box-signature-version'))
                || !'HmacSHA256'.equals(header(headers, 'box-signature-algorithm'))) {
            return false;
        }
        String timestamp = header(headers, 'box-delivery-timestamp');
        Long sent = unixSeconds(timestamp);
        if (sent == null) {
            return false;
        }
        // Whole seconds, as Box's own SDKs compare them.
        Long at = now.getTime() / 1000;
        if (sent > at || at - sent > maxAgeSeconds) {
            return false;
        }

        List<Blob> bodies = new List<Blob>{ body };
        String escaped = escapeNonAscii(body);
        if (escaped != null) {
            bodies.add(Blob.valueOf(escaped));
        }
        String stampHex = EncodingUtil.convertToHex(Blob.valueOf(timestamp));
        List<List<String>> signed = new List<List<String>>{
            new List<String>{ primaryKey, 'box-signature-primary' },
            new List<String>{ secondaryKey, 'box-signature-secondary' }
        };
        for (List<String> pair : signed) {
            String received = header(headers, pair[1]);
            if (String.isEmpty(pair[0]) || String.isEmpty(received)) {
                continue;
            }
            for (Blob candidate : bodies) {
                // Body bytes then timestamp bytes, joined byte-exact via hex.
                Blob message = EncodingUtil.convertFromHex(EncodingUtil.convertToHex(candidate) + stampHex);
                Blob mac = Crypto.generateMac('hmacSHA256', message, Blob.valueOf(pair[0]));
                if (sameString(EncodingUtil.base64Encode(mac), received)) {
                    return true;
                }
            }
        }
        return false;
    }

    private static String header(Map<String, String> headers, String name) {
        for (String key : headers.keySet()) {
            if (key != null && key.equalsIgnoreCase(name)) {
                return headers.get(key);
            }
        }
        return null;
    }

    /** Equality that takes the same time wherever the strings differ. */
    private static Boolean sameString(String a, String b) {
        if (a.length() != b.length()) {
            return false;
        }
        List<Integer> x = a.getChars();
        List<Integer> y = b.getChars();
        Integer diff = 0;
        for (Integer i = 0; i < x.size(); i++) {
            diff = diff | (x[i] ^ y[i]);
        }
        return diff == 0;
    }

    /** The Unix time, in whole seconds, of an RFC 3339 timestamp such as
     * `2020-01-01T00:00:00-07:00`; null when it isn't one. */
    @TestVisible
    private static Long unixSeconds(String timestamp) {
        if (timestamp == null) {
            return null;
        }
        Matcher m = RFC_3339.matcher(timestamp);
        if (!m.matches()) {
            return null;
        }
        Integer month = Integer.valueOf(m.group(2));
        Integer day = Integer.valueOf(m.group(3));
        Integer hour = Integer.valueOf(m.group(4));
        Integer minute = Integer.valueOf(m.group(5));
        Integer second = Integer.valueOf(m.group(6));
        if (month < 1 || month > 12 || day < 1 || day > 31 || hour > 23 || minute > 59 || second > 59) {
            return null;
        }
        Long seconds = Datetime.newInstanceGmt(Integer.valueOf(m.group(1)), month, day, hour, minute, second)
            .getTime() / 1000;
        if (m.group(7) == null) {
            Integer zoneHours = Integer.valueOf(m.group(9));
            Integer zoneMinutes = Integer.valueOf(m.group(10));
            if (zoneHours > 23 || zoneMinutes > 59) {
                return null;
            }
            Integer offset = zoneHours * 3600 + zoneMinutes * 60;
            seconds -= m.group(8) == '-' ? -offset : offset;
        }
        return seconds;
    }

    /** `body` with every character from U+007F up written as a JSON unicode
     * escape (UTF-16, lowercase hex) — the form Box signs. A body re-encoded as
     * raw UTF-8 on its way to the caller verifies through it. Null when `body`
     * is already all ASCII, or is not UTF-8. */
    @TestVisible
    private static String escapeNonAscii(Blob body) {
        String text;
        try {
            text = body.toString();
        } catch (Exception notUtf8) {
            return null;
        }
        List<String> out = new List<String>();
        Boolean escaped = false;
        Integer i = 0;
        while (i < text.length()) {
            Integer codePoint = text.codePointAt(i);
            Integer width = codePoint > 65535 ? 2 : 1;
            if (codePoint < 127) {
                out.add(text.substring(i, i + 1));
            } else if (width == 1) {
                escaped = true;
                out.add('\\u' + hex4(codePoint));
            } else {
                // A UTF-16 surrogate pair, as JSON escapes it.
                escaped = true;
                Integer offset = codePoint - 65536;
                out.add('\\u' + hex4(55296 + (offset >> 10)) + '\\u' + hex4(56320 + (offset & 1023)));
            }
            i += width;
        }
        return escaped ? String.join(out, '') : null;
    }

    private static String hex4(Integer unit) {
        String out = '';
        for (Integer shift = 12; shift >= 0; shift -= 4) {
            Integer digit = (unit >> shift) & 15;
            out += HEX_DIGITS.substring(digit, digit + 1);
        }
        return out;
    }
}
