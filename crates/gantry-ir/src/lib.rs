//! The typed intermediate representation (FR-2).
//!
//! Design rules, from `NEW_ENGINE_REQUIREMENTS.md` and the box-codegen
//! lessons (`REWRITE_ASSESSMENT.md` §2):
//!
//! - **Closed node set** (FR-2.1): every shape a backend must handle is a
//!   variant of an exhaustive enum. Adding a variant breaks every lowering
//!   that misses it, at compile time.
//! - **No semantics in strings** (FR-2.2): optionality, references, chains,
//!   and operation kinds are structured data. [`Identifier`] actively
//!   rejects the string conventions the old engine embedded in names
//!   (`.` chains, `!` force-unwraps, `#` variation markers).
//! - **Optionality is a type constructor** (FR-2.3): [`Type::Optional`],
//!   lowered per target (Go pointers / Apex nullable / Rust `Option<T>`).
//! - **References are resolved links** (FR-2.4): [`DeclId`] indexes into
//!   the [`Program`] arena. There is no "look this name up later".
//! - **Unions are rich** (FR-2.5): discriminator, per-variant values, and
//!   open/closed extensibility — enough for Rust enums, Go variant
//!   structs, and Apex parse dispatch.
//! - **Modules are a rich concept** (FR-2.6): Go/Rust use them directly;
//!   the Apex backend lowers them to outer-class grouping. Apex's flat
//!   namespace never shapes this model.

/// A single name segment: one identifier in one namespace.
///
/// This is deliberately *not* a path, a chain, or an expression. The old
/// engine encoded semantics in name strings (`"zipDownloadSession.downloadUrl!"`)
/// and every consumer re-parsed them slightly differently; construction of
/// an `Identifier` rejects those conventions outright (FR-2.2).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Identifier(String);

/// Characters that would smuggle semantics into a name string.
const FORBIDDEN: &[char] = &['.', '!', '#', '/', ' ', '\t', '\n'];

impl Identifier {
    /// Create an identifier, rejecting empty strings and any character that
    /// historically carried smuggled semantics.
    pub fn new(name: impl Into<String>) -> Result<Self, InvalidIdentifier> {
        let name = name.into();
        if name.is_empty() {
            return Err(InvalidIdentifier {
                name,
                reason: "empty",
            });
        }
        if name.contains(FORBIDDEN) {
            return Err(InvalidIdentifier {
                name,
                reason: "contains a character that encodes semantics ('.', '!', '#', '/', or whitespace)",
            });
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Error for [`Identifier::new`]; names the offending string (NF-3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidIdentifier {
    pub name: String,
    pub reason: &'static str,
}

impl std::fmt::Display for InvalidIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid identifier {:?}: {}", self.name, self.reason)
    }
}

impl std::error::Error for InvalidIdentifier {}

/// A module path: the rich namespace concept (FR-2.6).
///
/// Go lowers this to packages, Rust to modules, Apex to deterministic
/// outer-class grouping + name mangling (TR-Apex.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModulePath(pub Vec<Identifier>);

/// A resolved reference to a declaration in the [`Program`] arena (FR-2.4).
///
/// An unresolved reference cannot be represented: semantic analysis (FR-3)
/// either produces a `DeclId` or reports an error with a spec location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeclId(pub u32);

/// The closed set of types (FR-2.1, FR-2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Bool,
    /// 64-bit integer. Box APIs use int64 throughout.
    Int64,
    Float64,
    String,
    /// RFC 3339 date (no time component).
    Date,
    /// RFC 3339 date-time.
    DateTime,
    /// Raw binary content, streamed where the platform allows (FR-7.4).
    Binary,
    /// The optionality constructor (FR-2.3): present-or-absent, lowered per
    /// target. Never encoded as a marker inside a name string.
    Optional(Box<Type>),
    List(Box<Type>),
    /// String-keyed map with homogeneous values.
    Map(Box<Type>),
    /// A resolved reference to a named declaration (FR-2.4).
    Decl(DeclId),
    /// A schema-less JSON value (the spec says nothing about its shape).
    /// Lowered to each target's raw-JSON type; contents round-trip
    /// untouched. This is an explicit, deliberate hole — never a silent
    /// fallback for shapes the lowering failed to classify (NF-1).
    JsonValue,
}

/// A named declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decl {
    pub name: Identifier,
    pub module: ModulePath,
    /// API version this declaration belongs to (e.g. `2025.0` — FR-7.5).
    pub api_version: Option<ApiVersion>,
    pub kind: DeclKind,
}

/// The closed set of declaration kinds (FR-2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclKind {
    Struct(StructDecl),
    Union(UnionDecl),
    Enum(EnumDecl),
    Alias(Type),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDecl {
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: Identifier,
    /// The wire name (JSON key) — kept separate from the identifier so
    /// naming rules never round-trip through serialization names.
    pub wire_name: String,
    pub ty: Type,
}

/// A discriminated union (FR-2.5, D-012, G-10/G-11).
///
/// Rich enough that Rust lowers it to a serde-tagged enum (TR-Rust.1), Go
/// to variant structs with generated `MarshalJSON`/`UnmarshalJSON`
/// (TR-Go.2), and Apex to generated `JSON.deserializeUntyped` dispatch
/// (TR-Apex.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnionDecl {
    /// The wire field whose value selects the variant, when one exists.
    pub discriminator: Option<String>,
    pub variants: Vec<UnionVariant>,
    /// Open unions must retain and round-trip unknown discriminator values
    /// (G-10, VR-4); closed unions may reject them.
    pub extensibility: Extensibility,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnionVariant {
    /// The discriminator value that selects this variant, when the union
    /// has a discriminator.
    pub discriminator_value: Option<String>,
    pub ty: Type,
}

/// An enum of string values (Box's extensible enums, G-11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumDecl {
    pub values: Vec<String>,
    /// Open enums retain unknown values and round-trip them (D-012).
    pub extensibility: Extensibility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extensibility {
    /// Unknown values are retained and round-tripped.
    Open,
    /// Unknown values are a deserialization error.
    Closed,
}

/// Semantic version tag of a Box API surface (e.g. `2025.0`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ApiVersion(pub String);

/// The closed set of HTTP methods OpenAPI 3.x defines (FR-2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Put,
    Post,
    Delete,
    Options,
    Head,
    Patch,
    Trace,
}

/// The Box base-URL classes an operation can target (G-2). The mapping
/// from spec `servers` URLs to these classes is fixed at ingestion
/// (D-106); an unknown URL is a loud error, never a pass-through string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseUrl {
    /// `https://api.box.com/2.0` — the default.
    Api,
    /// `https://api.box.com` — OAuth2 token/revoke endpoints.
    ApiRoot,
    /// `https://upload.box.com/api/2.0` — multipart uploads.
    Upload,
    /// `https://{box-upload-server}/api/2.0` — the session-provided host
    /// for chunked-upload parts.
    UploadSession,
    /// `https://account.box.com/api/oauth2` — the authorize web page.
    OAuthAuthorize,
    /// `https://dl.boxcloud.com/2.0` — direct downloads.
    Download,
}

/// One segment of a request path — structure, not a template string to
/// re-parse (FR-2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSegment {
    Literal(String),
    /// Filled from the path parameter with the same name (validated at
    /// ingestion).
    Parameter(Identifier),
    /// A segment mixing literal text and parameters, e.g. the real spec's
    /// `thumbnail.{extension}`.
    Composite(Vec<PathPart>),
}

/// One piece of a [`PathSegment::Composite`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathPart {
    Literal(String),
    Parameter(Identifier),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParamLocation {
    Path,
    Query,
    Header,
}

/// One operation parameter. Optionality is in the type (FR-2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: Identifier,
    /// The wire name (query key / header name), kept apart from the
    /// identifier just like struct fields.
    pub wire_name: String,
    pub location: ParamLocation,
    pub ty: Type,
}

/// The closed set of request media the Box specs use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestMedia {
    Json,
    /// `application/json-patch+json` (metadata updates).
    JsonPatch,
    UrlEncoded,
    Multipart,
    OctetStream,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestBody {
    pub media: RequestMedia,
    /// `Optional<…>` when the spec marks the body itself as not required.
    pub ty: Type,
}

/// What a successful call yields — classified at ingestion, never
/// re-derived from status-code strings by backends (FR-2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseShape {
    /// Success carries no body (e.g. 204).
    None,
    Json(Type),
    /// Raw bytes (downloads, thumbnails) — streamed where the platform
    /// allows (FR-7.4).
    Binary,
    /// Textual non-JSON content (the authorize page).
    Text,
    /// Success is a redirect; the result is the `Location` URL.
    Redirect,
}

/// One API operation, fully resolved (FR-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    /// The base operation name; any `#variation` suffix from the spec's
    /// `operationId` is split into [`Operation::variation`] — `#` never
    /// reaches an identifier.
    pub name: Identifier,
    pub variation: Option<Identifier>,
    /// Manager grouping key (from `x-box-tag`, D-104; FR-7.1).
    pub manager: Identifier,
    pub api_version: Option<ApiVersion>,
    pub method: HttpMethod,
    pub base_url: BaseUrl,
    pub path: Vec<PathSegment>,
    pub params: Vec<Param>,
    pub request: Option<RequestBody>,
    pub response: ResponseShape,
    pub deprecated: bool,
}

/// A verified program: the only thing a backend ever receives (FR-3.2).
///
/// All `DeclId`s index into `decls`; the arena is append-only during
/// construction so ids are stable.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Program {
    pub decls: Vec<Decl>,
    pub operations: Vec<Operation>,
}

impl Program {
    /// Resolve an id. Panics on an out-of-range id — that is an engine bug
    /// (a `DeclId` that didn't come from this program), and engine bugs
    /// fail loudly (NF-1), never emit placeholders (FR-3.2).
    pub fn decl(&self, id: DeclId) -> &Decl {
        &self.decls[id.0 as usize]
    }

    pub fn add(&mut self, decl: Decl) -> DeclId {
        let id = DeclId(u32::try_from(self.decls.len()).expect("more than u32::MAX declarations"));
        self.decls.push(decl);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_rejects_smuggled_semantics() {
        // The exact conventions the old engine embedded in name strings.
        for bad in [
            "zipDownloadSession.downloadUrl",
            "downloadUrl!",
            "post_oauth2_token#refresh",
            "",
        ] {
            assert!(Identifier::new(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(Identifier::new("downloadUrl").is_ok());
    }

    #[test]
    fn decl_ids_resolve_in_the_arena() {
        let mut program = Program::default();
        let inner = program.add(Decl {
            name: Identifier::new("FileMini").unwrap(),
            module: ModulePath(vec![Identifier::new("schemas").unwrap()]),
            api_version: None,
            kind: DeclKind::Struct(StructDecl { fields: vec![] }),
        });
        let outer = program.add(Decl {
            name: Identifier::new("Folder").unwrap(),
            module: ModulePath(vec![Identifier::new("schemas").unwrap()]),
            api_version: None,
            kind: DeclKind::Struct(StructDecl {
                fields: vec![Field {
                    name: Identifier::new("file").unwrap(),
                    wire_name: "file".into(),
                    // Optionality is structure, not a `!` suffix.
                    ty: Type::Optional(Box::new(Type::Decl(inner))),
                }],
            }),
        });
        assert_eq!(program.decl(outer).name.as_str(), "Folder");
        assert_eq!(program.decl(inner).name.as_str(), "FileMini");
    }
}
