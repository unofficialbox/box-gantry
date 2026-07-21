// Hand-written Box Apex runtime (TR-Apex) — tests for the auth facade.
//
// Each factory returns the matching BoxTokenProvider; construction validates
// its arguments (the providers throw on blanks), so building one with valid
// arguments proves the facade wires the flow through.
@isTest
private class BoxAuthTest {
    @isTest
    static void developerTokenBuildsAProvider() {
        BoxTokenProvider p = BoxAuth.developerToken('DEV_TOKEN');
        System.assertNotEquals(null, p, 'developer token provider');
    }

    @isTest
    static void clientCredentialsBuildsAProvider() {
        BoxTokenProvider p = BoxAuth.clientCredentials(
            'CLIENT_ID',
            'CLIENT_SECRET',
            'enterprise',
            'ENTERPRISE_ID'
        );
        System.assertNotEquals(null, p, 'CCG provider');
    }

    @isTest
    static void jwtBuildsAProvider() {
        BoxTokenProvider p = BoxAuth.jwt(
            'CLIENT_ID',
            'CLIENT_SECRET',
            'enterprise',
            'ENTERPRISE_ID',
            'PUBLIC_KEY_ID',
            'Box_JWT'
        );
        System.assertNotEquals(null, p, 'JWT provider');
    }
}
