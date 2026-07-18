//! VR-4 (Java): the generated JSON codec (D-172) doesn't just *compile* — it
//! *round-trips*. This test generates a small SDK covering the shapes that
//! matter (the absent/null/value tri-state, a plain optional, a bare nullable,
//! a list, a nested declaration, a closed enum, and an open discriminated
//! union), compiles it with the real `javac`, and then *runs* a driver under
//! `java` that decodes a document, inspects the reconstructed values, and
//! re-encodes — proving the wire mapping is correct, not merely well-typed.
//!
//! Skips cleanly when the JDK is absent (local dev); CI installs one.

use std::path::{Path, PathBuf};
use std::process::Command;

use gantry_ir::{
    Decl, DeclKind, EnumDecl, Extensibility, Field, Identifier, ModulePath, Program, StructDecl,
    Type, UnionDecl, UnionVariant,
};

fn ident(s: &str) -> Identifier {
    Identifier::new(s).unwrap()
}

fn schemas() -> ModulePath {
    ModulePath(vec![ident("schemas")])
}

fn add(program: &mut Program, name: &str, kind: DeclKind) -> gantry_ir::DeclId {
    program.add(Decl {
        name: ident(name),
        module: schemas(),
        api_version: None,
        kind,
    })
}

fn field(name: &str, wire: &str, ty: Type) -> Field {
    Field {
        name: ident(name),
        wire_name: wire.into(),
        ty,
    }
}

/// A program exercising every codec path the driver then checks at run time.
fn build_program() -> Program {
    let mut p = Program::default();
    // Two discriminator-carrying variant structs → a typed sealed union.
    let dog = add(
        &mut p,
        "Dog",
        DeclKind::Struct(StructDecl {
            fields: vec![
                field("kind", "kind", Type::String),
                field("bark", "bark", Type::String),
            ],
        }),
    );
    let cat = add(
        &mut p,
        "Cat",
        DeclKind::Struct(StructDecl {
            fields: vec![
                field("kind", "kind", Type::String),
                field("meows", "meows", Type::Optional(Box::new(Type::Int64))),
            ],
        }),
    );
    let pet = add(
        &mut p,
        "Pet",
        DeclKind::Union(UnionDecl {
            discriminator: Some("kind".into()),
            variants: vec![
                UnionVariant {
                    discriminator_value: Some("dog".into()),
                    ty: Type::Decl(dog),
                },
                UnionVariant {
                    discriminator_value: Some("cat".into()),
                    ty: Type::Decl(cat),
                },
            ],
            extensibility: Extensibility::Open,
        }),
    );
    let color = add(
        &mut p,
        "Color",
        DeclKind::Enum(EnumDecl {
            values: vec!["red".into(), "green".into()],
            extensibility: Extensibility::Closed,
        }),
    );
    add(
        &mut p,
        "Widget",
        DeclKind::Struct(StructDecl {
            fields: vec![
                field("id", "id", Type::String),
                field("name", "name", Type::Optional(Box::new(Type::String))),
                field(
                    "size",
                    "size",
                    Type::Optional(Box::new(Type::Nullable(Box::new(Type::Int64)))),
                ),
                field("note", "note", Type::Nullable(Box::new(Type::String))),
                field("tags", "tags", Type::List(Box::new(Type::String))),
                field("pet", "pet", Type::Decl(pet)),
                field("color", "color", Type::Decl(color)),
            ],
        }),
    );
    p
}

/// The `java` driver: decode a document, assert the reconstructed tri-state /
/// optional / nullable / union values, re-encode, and assert the wire shape —
/// including that an *unknown* discriminator round-trips through `Unknown`
/// (VR-4). Uses fully-qualified names so it needs no package of its own.
const DRIVER: &str = r#"
public final class Main {
    static void check(boolean cond, String msg) {
        if (!cond) {
            System.out.println("FAIL: " + msg);
            System.exit(1);
        }
    }

    public static void main(String[] args) {
        String in = "{\"id\":\"x\",\"size\":null,\"note\":null,\"tags\":[\"a\",\"b\"],"
            + "\"pet\":{\"kind\":\"dog\",\"bark\":\"woof\"},\"color\":\"red\"}";
        com.box.sdk.model.schemas.Widget w =
            com.box.sdk.model.schemas.Widget.fromJson(com.box.sdk.core.Json.parse(in));

        // Tri-state: an explicit wire null decodes to NULL, not absent.
        check(w.size().isNull(), "size should be explicit null");
        // A plain optional that was omitted decodes to empty.
        check(w.name().isEmpty(), "name should be absent");
        // A bare nullable decodes to a null reference.
        check(w.note() == null, "note should be null");
        check(w.tags().size() == 2 && w.tags().get(0).equals("a"), "tags");
        check(w.color() == com.box.sdk.model.schemas.Color.RED, "color");
        check(w.pet() instanceof com.box.sdk.model.schemas.Dog, "pet dispatches to Dog");

        String out = com.box.sdk.core.Json.write(w.toJson());
        // An explicit null re-encodes as null; an absent field is omitted.
        check(out.contains("\"size\":null"), "size null must re-encode: " + out);
        check(!out.contains("\"name\""), "absent name must be omitted: " + out);
        check(out.contains("\"kind\":\"dog\""), "union tag must round-trip: " + out);
        check(out.contains("\"color\":\"red\""), "enum wire value must round-trip: " + out);

        // An unrecognized discriminator on an open union is retained verbatim.
        com.box.sdk.model.schemas.Pet unknown = com.box.sdk.model.schemas.Pet.fromJson(
            com.box.sdk.core.Json.parse("{\"kind\":\"bird\",\"wings\":2}"));
        check(unknown instanceof com.box.sdk.model.schemas.Pet.Unknown, "unknown discriminator retained");
        check(com.box.sdk.core.Json.write(unknown.toJson()).contains("\"wings\":2"),
            "unknown payload must round-trip");

        System.out.println("ROUNDTRIP_OK");
    }
}
"#;

/// Write every generated file; return only the `.java` sources (reference-doc
/// Markdown is written too, but must not be handed to `javac`).
fn write_all(dir: &Path, files: &[gantry_backend_java::GeneratedFile]) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    for file in files {
        let path = dir.join(&file.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &file.content).unwrap();
        if file.path.ends_with(".java") {
            sources.push(path);
        }
    }
    sources
}

#[test]
fn the_generated_codec_round_trips_under_java() {
    if Command::new("javac").arg("-version").output().is_err()
        || Command::new("java").arg("-version").output().is_err()
    {
        eprintln!("SKIPPED: JDK not available; CI installs one and runs this gate");
        return;
    }

    let program = build_program();
    let analysis = gantry_sema::analyze(&program).expect("synthetic program is well-formed");
    let build = gantry_backend_java::BuildInfo::new("roundtripfp");
    let files = gantry_backend_java::generate(&analysis, &gantry_manifest::java(), &build);

    let dir = std::env::temp_dir().join(format!("gantry-java-rt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut sources = write_all(&dir, &files);
    let main = dir.join("Main.java");
    std::fs::write(&main, DRIVER).unwrap();
    sources.push(main);

    let argfile = dir.join("sources.txt");
    let listing: String = sources
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&argfile, listing).unwrap();
    let classes = dir.join("classes");
    std::fs::create_dir_all(&classes).unwrap();

    let javac = Command::new("javac")
        .arg("--release")
        .arg("26")
        .arg("-Xlint:all")
        .arg("-Werror")
        .arg("-d")
        .arg(&classes)
        .arg(format!("@{}", argfile.display()))
        .output()
        .unwrap();
    assert!(
        javac.status.success(),
        "javac failed:\n{}{}",
        String::from_utf8_lossy(&javac.stdout),
        String::from_utf8_lossy(&javac.stderr)
    );

    let run = Command::new("java")
        .arg("-cp")
        .arg(&classes)
        .arg("Main")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        run.status.success() && stdout.contains("ROUNDTRIP_OK"),
        "round-trip driver failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
}
