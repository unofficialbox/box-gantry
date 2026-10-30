// Hand-written Box Apex runtime (TR-Apex.6) — acting as another user.
//
// Wraps any `BoxClient` and sends `As-User: <userId>` with every request, so
// Box acts as that user (an admin, or a service account whose app may "make
// API calls using the as-user header"). The generated `Box.withAsUserHeader`
// builds one; a decorator rather than a `BoxClient` method, so custom
// `BoxClient` implementations (mocks, other transports) keep compiling. Token
// requests never pass through a `BoxClient`, so they never carry the header.
public with sharing class BoxAsUserClient implements BoxClient {
    private final BoxClient wrapped;
    private final String userId;

    public BoxAsUserClient(BoxClient wrapped, String userId) {
        if (wrapped == null || String.isBlank(userId)) {
            throw new BoxApiException('A client and a user id are required.');
        }
        // Re-deriving replaces the user: wrap the underlying client, not a
        // decorator whose own header would overwrite this one.
        this.wrapped = wrapped instanceof BoxAsUserClient
            ? ((BoxAsUserClient) wrapped).wrapped
            : wrapped;
        this.userId = userId;
    }

    public BoxResponse send(BoxRequest request) {
        for (String name : new List<String>(request.headers.keySet())) {
            if (name.equalsIgnoreCase('As-User')) {
                request.headers.remove(name);
            }
        }
        request.headers.put('As-User', this.userId);
        return this.wrapped.send(request);
    }
}
