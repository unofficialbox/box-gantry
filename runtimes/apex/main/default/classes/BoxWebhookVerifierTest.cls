// Hand-written Box Apex runtime (TR-Apex.6) — BoxWebhookVerifier against the
// vectors from Box's own SDK test suite (testWebhookValidation): keys, bodies,
// and the signatures Box computed for them, all delivered at
// 2020-01-01T00:00:00-07:00 — and every way a delivery must be refused.
@isTest
private class BoxWebhookVerifierTest {
    private static final String PRIMARY_KEY = 'SamplePrimaryKey';
    private static final String SECONDARY_KEY = 'SampleSecondaryKey';
    private static final String BODY = '{"type":"webhook_event","webhook":{"id":"1234567890"},"trigger":"FILE.UPLOADED","source":{"id":"1234567890","type":"file","name":"Test.txt"}}';
    private static final String PRIMARY = '6TfeAW3A1PASkgboxxA5yqHNKOwFyMWuEXny/FPD5hI=';
    private static final String FILE_PREFIX = '{"webhook":{"id":"1234567890"},"trigger":"FILE.UPLOADED","source":{"id":"1234567890","type":"file","name":"';
    private static final Datetime DELIVERED = Datetime.newInstanceGmt(2020, 1, 1, 7, 0, 0);

    // Mixed case on purpose: lookups must not depend on the sender's casing.
    private static Map<String, String> headers(String primary) {
        return new Map<String, String>{
            'box-delivery-id' => 'f96bb54b-ee16-4fc5-aa65-8c2d9e5b546f',
            'BOX-DELIVERY-TIMESTAMP' => '2020-01-01T00:00:00-07:00',
            'box-signature-algorithm' => 'HmacSHA256',
            'box-signature-primary' => primary,
            'Box-Signature-Secondary' => 'v+1CD1Jdo3muIcbpv5lxxgPglOqMfsNHPV899xWYydo=',
            'box-signature-version' => '1'
        };
    }

    private static Map<String, String> withHeader(String name, String value) {
        Map<String, String> h = headers(PRIMARY);
        for (String key : new List<String>(h.keySet())) {
            if (key.equalsIgnoreCase(name)) {
                h.remove(key);
            }
        }
        if (value != null) {
            h.put(name, value);
        }
        return h;
    }

    private static BoxWebhookVerifier both() {
        return new BoxWebhookVerifier(PRIMARY_KEY, SECONDARY_KEY);
    }

    @isTest
    static void acceptsTheSignaturesBoxComputed() {
        Blob payload = Blob.valueOf(BODY);
        System.assert(both().verify(payload, headers(PRIMARY), DELIVERED), 'both keys verify');
        System.assert(
            new BoxWebhookVerifier(null, SECONDARY_KEY).verify(payload, headers('not-the-primary'), DELIVERED),
            'the secondary key alone verifies, as during a primary rotation'
        );
        System.assert(
            both().verify(payload, headers(PRIMARY), Datetime.newInstanceGmt(2020, 1, 1, 7, 10, 0)),
            'exactly the maximum age old is still in time'
        );

        // Box signs non-ASCII characters as JSON unicode escapes; a body that
        // arrives as raw UTF-8 verifies through that form. Escapes already in
        // the JSON (backslash-r, backslash-slash, a doubled backslash) are
        // signed as they are. The non-ASCII names are built from UTF-8 hex so
        // this file stays ASCII.
        Map<String, String> signed = new Map<String, String>{
            EncodingUtil.convertFromHex('e382b9e382afe383aae383bce383b3e382b7e383a7e38383e3838820323032302d30382d30352e747874').toString()
                => 'LV2uCu+5NJtIHrCXDYgZ0v/PP5THGRuegw3RtdnEyuE=',
            EncodingUtil.convertFromHex('f09f988020323032302d30382d30352e747874').toString()
                => 'xF/SDZosX4le+v4A0Qn59sZhuD1RqY5KRUKzVMSbh0E=',
            'test \\r' => 'SVkbKgy3dEEf2PbbzpNu2lDZS7zZ/aboU7HOZgBGrJk=',
            '\\/' => 't41PWT5ZB6OcysnD6SDy9Ud+p9hdXxIdXqcdweyZv/Q=',
            '\\\\' => 'ERpMZwUQsGDTfj82ehdX6VvDZfvOhK5ULNfVmwVAGe0='
        };
        BoxWebhookVerifier primaryOnly = new BoxWebhookVerifier(PRIMARY_KEY, null);
        for (String name : signed.keySet()) {
            Blob named = Blob.valueOf(FILE_PREFIX + name + '"}}');
            System.assert(primaryOnly.verify(named, headers(signed.get(name)), DELIVERED), name + ': Box\'s signature verifies');
        }
    }

    @isTest
    static void rejectsWhatBoxDidNotSend() {
        Blob payload = Blob.valueOf(BODY);
        Map<String, String> unsigned = withHeader('box-signature-primary', null);
        unsigned.remove('Box-Signature-Secondary');
        BoxWebhookVerifier shortLived = both();
        shortLived.maxAgeSeconds = 60;

        System.assert(!new BoxWebhookVerifier('IncorrectKey', 'IncorrectKey').verify(payload, headers(PRIMARY), DELIVERED), 'wrong keys');
        System.assert(!new BoxWebhookVerifier(null, null).verify(payload, headers(PRIMARY), DELIVERED), 'no keys');
        System.assert(!new BoxWebhookVerifier(SECONDARY_KEY, null).verify(payload, headers(PRIMARY), DELIVERED), 'secondary key for primary');
        System.assert(!both().verify(Blob.valueOf(BODY.replace('Test.txt', 'Test.exe')), headers(PRIMARY), DELIVERED), 'tampered body');
        System.assert(!both().verify(payload, headers(PRIMARY), Datetime.newInstanceGmt(2020, 1, 1, 7, 10, 1)), 'stale');
        System.assert(!both().verify(payload, headers(PRIMARY), Datetime.newInstanceGmt(2020, 1, 1, 6, 59, 59)), 'future');
        System.assert(!shortLived.verify(payload, headers(PRIMARY), Datetime.newInstanceGmt(2020, 1, 1, 7, 1, 1)), 'short max age');
        System.assert(!both().verify(payload, withHeader('box-delivery-timestamp', null), DELIVERED), 'no timestamp');
        System.assert(!both().verify(payload, withHeader('box-delivery-timestamp', 'yesterday'), DELIVERED), 'bad timestamp');
        System.assert(!both().verify(payload, unsigned, DELIVERED), 'no signatures');
        System.assert(!both().verify(payload, withHeader('box-signature-version', null), DELIVERED), 'no version');
        System.assert(!both().verify(payload, withHeader('box-signature-version', '2'), DELIVERED), 'version 2');
        System.assert(!both().verify(payload, withHeader('box-signature-algorithm', 'HmacSHA1'), DELIVERED), 'HmacSHA1');
        System.assert(!both().verify(payload, withHeader('box-signature-algorithm', 'hmacsha256'), DELIVERED), 'algorithm case matters');
        System.assert(!both().verify(null, headers(PRIMARY), DELIVERED), 'no body');
        System.assert(!both().verify(payload, headers(PRIMARY)), 'a 2020 delivery is stale against the real clock');
    }

    @isTest
    static void parsesRfc3339Timestamps() {
        System.assertEquals(0L, BoxWebhookVerifier.unixSeconds('1970-01-01T00:00:00Z'));
        System.assertEquals(1577862000L, BoxWebhookVerifier.unixSeconds('2020-01-01T00:00:00-07:00'));
        System.assertEquals(1577862000L, BoxWebhookVerifier.unixSeconds('2020-01-01T07:00:00.250Z'));
        System.assertEquals(1709190000L, BoxWebhookVerifier.unixSeconds('2024-02-29T12:30:00+05:30'));
        for (String bad : new List<String>{ null, '', 'yesterday', '2020-01-01 00:00:00Z', '2020-13-01T00:00:00Z', '2020-01-01T00:00:00', '2020-01-01T00:00:00+0700' }) {
            System.assertEquals(null, BoxWebhookVerifier.unixSeconds(bad), 'not RFC 3339: ' + bad);
        }
        System.assertEquals(null, BoxWebhookVerifier.escapeNonAscii(Blob.valueOf('plain ascii')), 'all ASCII needs no escaped form');
    }
}
