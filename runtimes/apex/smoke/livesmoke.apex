// Box Apex live smoke (VR-1.4) — the on-platform runtime proof.
//
// Runs as ANONYMOUS Apex against a real scratch org with the generated SDK
// deployed. It is deliberately not a test: Apex unit tests cannot make real
// callouts (they must mock), so the only way to prove the generated SDK works
// end-to-end against live Box is to run it outside the test context.
//
// It mints a token via Client Credentials Grant (no user in the loop), then two
// live reads exercise, in one flow: auth (a real token mint), URL building, the
// bearer header, native typed deserialization, the wire key-remap, and
// pagination. The `${...}` placeholders are filled from repo secrets by the
// workflow — this file never contains real credentials.
BoxTokenProvider auth = new BoxCcgTokenProvider(
    '${BOX_CLIENT_ID}',
    '${BOX_CLIENT_SECRET}',
    'enterprise',
    '${BOX_ENTERPRISE_ID}'
);
Box client = new Box(new BoxHttpClient(auth));

// 1) Auth + a typed GET: GET /users/me → UserFull (the CCG service account).
//    Proves the token mint, the bearer header, and native deserialization.
UserFull me = client.users.getMe(null);
System.assertNotEquals(null, me, 'getMe returned null');
System.assert(String.isNotBlank(me.id), 'getMe returned no user id');
System.debug(LoggingLevel.ERROR, 'SMOKE getMe id=' + me.id + ' login=' + me.login);

// 2) Pagination + wire remap: GET /folders/0/items → Items. `Items` renames
//    `limit` → `limit_r`, so this also drives normalizeKeys on a real response.
Items root = client.folders.getItems('0', null, false, null, null, null, null, null, null);
System.assertNotEquals(null, root, 'getItems returned null');
Integer entryCount = (root.entries == null) ? 0 : root.entries.size();
System.debug(
    LoggingLevel.ERROR,
    'SMOKE getItems entries=' + entryCount + ' total=' + root.total_count
);

// The sentinel the workflow greps for — reached only if every call above
// succeeded and no assertion or callout threw.
System.debug(LoggingLevel.ERROR, 'SMOKE OK');
