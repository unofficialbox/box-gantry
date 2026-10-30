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
// pagination. A scratch folder and a scratch web link then carry Box's etag
// flow (304 and 412), and the folder a zip download and a downscoped token. The `${...}` placeholders are filled from repo secrets by
// the workflow — this file never contains real credentials.
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
Items root = client.folders.listItems('0', null, false, null, null, null, null, null, null);
System.assertNotEquals(null, root, 'listItems returned null');
Integer entryCount = (root.entries == null) ? 0 : root.entries.size();
System.debug(
    LoggingLevel.ERROR,
    'SMOKE listItems entries=' + entryCount + ' total=' + root.total_count
);

// 3) Box's etag flow through the generated methods, on a scratch folder that is
//    deleted afterwards: getIfModified with the current etag is null (Box's
//    304) and with a stale one the folder; an update with a stale If-Match
//    throws the 412 that isPreconditionFailed() reports, and the current etag
//    writes. Box's etags are version counters, so '9' + etag is well-formed
//    but stale.
CreateFolderRequest scratch = new CreateFolderRequest();
scratch.name = 'box-gantry-smoke-' + Datetime.now().getTime();
scratch.parent = new AttributesParent();
scratch.parent.id = '0';
FolderFull folder = client.folders.create(null, scratch);
try {
    String etag = folder.etag;
    String stale = '9' + etag;
    System.assert(String.isNotBlank(etag), 'the new folder has no etag');
    System.assertEquals(
        null,
        client.folders.getIfModified(folder.id, etag, null, null, null, null, null, null),
        'getIfModified with the current etag should be null (304)'
    );
    System.assertNotEquals(
        null,
        client.folders.getIfModified(folder.id, stale, null, null, null, null, null, null),
        'getIfModified with a stale etag should read the folder'
    );
    UpdateFolderRequest change = new UpdateFolderRequest();
    change.description = 'box-gantry etag smoke';
    try {
        client.folders.update_r(folder.id, null, stale, change);
        System.assert(false, 'an update with a stale If-Match should throw');
    } catch (BoxApiException e) {
        System.assert(e.isPreconditionFailed(), 'a stale If-Match should be a 412: ' + e.getMessage());
    }
    client.folders.update_r(folder.id, null, etag, change);
    System.debug(LoggingLevel.ERROR, 'SMOKE etags folder=' + folder.id + ' etag=' + etag);

    // One-call zip download: downloadZip creates the zip and fetches the
    // download_url Box answers with, through this client. The scratch folder
    // is empty, so the archive may hold no entries; it must still be a zip.
    ZipDownloadRequestItems packed = new ZipDownloadRequestItems();
    packed.type = 'folder';
    packed.id = folder.id;
    ZipDownloadRequest zipRequest = new ZipDownloadRequest();
    zipRequest.items = new List<ZipDownloadRequestItems>{ packed };
    zipRequest.download_file_name = 'box-gantry-smoke';
    Blob archive = client.zipDownloads.downloadZip(zipRequest);
    System.assert(
        archive != null && EncodingUtil.convertToHex(archive).startsWith('504b'),
        'downloadZip should answer a zip archive'
    );
    System.debug(LoggingLevel.ERROR, 'SMOKE zip bytes=' + archive.size());

    // Token downscoping: exchange the session's token for one limited to
    // renaming and previewing the scratch folder. The narrow token renames it
    // and Box refuses its delete; the cleanup below uses the original token.
    List<String> scopes = new List<String>{ 'item_rename', 'item_preview' };
    AccessToken down = client.authorization.downscopeToken(
        auth.getAccessToken(),
        scopes,
        'https://api.box.com/2.0/folders/' + folder.id,
        null
    );
    System.assert(String.isNotBlank(down.access_token), 'the exchange returned no access token');
    Box narrow = new Box(new BoxHttpClient(BoxAuth.developerToken(down.access_token)));
    UpdateFolderRequest rename = new UpdateFolderRequest();
    rename.name = 'box-gantry-smoke-downscoped-' + Datetime.now().getTime();
    narrow.folders.update_r(folder.id, null, null, rename);
    try {
        narrow.folders.delete_r(folder.id, null, true);
        System.assert(false, 'a delete with the downscoped token should be refused');
    } catch (BoxApiException e) {
        System.assert(
            e.statusCode == 401 || e.statusCode == 403,
            'a delete with the downscoped token should be a 401/403: ' + e.getMessage()
        );
        System.debug(LoggingLevel.ERROR, 'SMOKE downscope refused delete: ' + e.getMessage());
    }
} finally {
    client.folders.delete_r(folder.id, null, true);
}

// 4) The same flow on a scratch web link. The upstream spec declares no etag
//    headers for web links; fixtures/specs/supplements.json adds them, and
//    this proves Box honors them.
CreateWebLinkRequest bookmark = new CreateWebLinkRequest();
bookmark.url = 'https://example.com/';
bookmark.name = 'box-gantry-smoke-' + Datetime.now().getTime();
bookmark.parent = new AttributesParent();
bookmark.parent.id = '0';
WebLink link = client.webLinks.create(bookmark);
try {
    String etag = link.etag;
    String stale = '9' + etag;
    System.assert(String.isNotBlank(etag), 'the new web link has no etag');
    System.assertEquals(
        null,
        client.webLinks.getIfModified(link.id, etag, null),
        'webLinks.getIfModified with the current etag should be null (304)'
    );
    System.assertNotEquals(
        null,
        client.webLinks.getIfModified(link.id, stale, null),
        'webLinks.getIfModified with a stale etag should read the web link'
    );
    UpdateWebLinkRequest change = new UpdateWebLinkRequest();
    change.description = 'box-gantry etag smoke';
    try {
        client.webLinks.update_r(link.id, stale, change);
        System.assert(false, 'a web link update with a stale If-Match should throw');
    } catch (BoxApiException e) {
        System.assert(e.isPreconditionFailed(), 'a stale If-Match should be a 412: ' + e.getMessage());
    }
    client.webLinks.update_r(link.id, etag, change);
    System.debug(LoggingLevel.ERROR, 'SMOKE web link etags link=' + link.id + ' etag=' + etag);
} finally {
    client.webLinks.delete_r(link.id, null);
}

// 5) The polled event stream: one poll from "now" fetches the events since and
//    advances the position Box returns (Apex can't hold Box's long poll open).
BoxEventStream events = client.events.stream(null, null, null, null, null, null);
List<Object> polled = events.poll();
System.assertNotEquals('now', events.position, 'the poll took the position Box returned');
System.debug(LoggingLevel.ERROR, 'SMOKE events polled=' + polled.size() + ' position=' + events.position);

// 6) Acting as a managed user through Box.withAsUserHeader: /users/me must
//    answer as that user. It needs an app allowed to "make API calls using the
//    as-user header" and a managed user; without them Box answers 403 or lists
//    no one, and the step logs SKIPPED instead of failing.
try {
    Users managed = client.users.list_r(null, 'managed', null, null, null, 100, null, null);
    String target;
    for (UserFull candidate : managed.entries == null ? new List<UserFull>() : managed.entries) {
        if (candidate.status == 'active' && candidate.id != me.id) {
            target = candidate.id;
            break;
        }
    }
    if (target == null) {
        System.debug(LoggingLevel.ERROR, 'SMOKE as-user SKIPPED no other active managed user');
    } else {
        Box acting = client.withAsUserHeader(target);
        UserFull actedAs = acting.users.getMe(null);
        System.assertEquals(target, actedAs.id, 'getMe as the user answers as that user');
        System.debug(LoggingLevel.ERROR, 'SMOKE as-user acted as ' + target);
    }
} catch (BoxApiException e) {
    if (e.statusCode != 403) {
        throw e;
    }
    System.debug(LoggingLevel.ERROR, 'SMOKE as-user SKIPPED 403 ' + e.code);
}

// 7) A representation through files.getRepresentationContent, on a scratch
//    .txt file that is deleted afterwards. Apex can't wait while Box generates
//    the extracted text, so the step tries a bounded number of times and logs
//    PENDING if Box hasn't finished; once Box has it, it must hold the uploaded
//    text. When Box has no extracted text for the file, the step logs SKIPPED.
String uploadedText = 'box-gantry live smoke';
CreateFileContentRequest textFile = new CreateFileContentRequest();
textFile.attributes = new PostFileContentAttributes();
textFile.attributes.name = 'box-gantry-smoke-' + Datetime.now().getTime() + '.txt';
textFile.attributes.parent = new AttributesParent();
textFile.attributes.parent.id = '0';
textFile.file = Blob.valueOf(uploadedText);
String textFileId = client.uploads.uploadFile(null, null, textFile).entries[0].id;
try {
    BoxRepresentations.Result text;
    for (Integer attempt = 0; attempt < 5 && (text == null || text.content == null); attempt++) {
        text = client.files.getRepresentationContent(textFileId, '[extracted_text]', '');
    }
    if (text.content == null) {
        System.debug(LoggingLevel.ERROR, 'SMOKE representation PENDING after 5 checks file=' + textFileId);
    } else {
        System.assert(
            text.content.toString().contains(uploadedText),
            'the extracted text should hold the uploaded text: ' + text.content.toString()
        );
        System.debug(LoggingLevel.ERROR, 'SMOKE representation extracted_text bytes=' + text.content.size());
    }
} catch (BoxApiException e) {
    String message = e.getMessage();
    if (!message.contains('no representation matches') && !message.contains('could not generate')) {
        throw e;
    }
    System.debug(LoggingLevel.ERROR, 'SMOKE representation SKIPPED ' + message);
} finally {
    client.files.delete_r(textFileId, null);
}

// The sentinel the workflow greps for — reached only if every call above
// succeeded and no assertion or callout threw.
System.debug(LoggingLevel.ERROR, 'SMOKE OK');
