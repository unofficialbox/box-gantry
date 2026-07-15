//! VR-4 for Rust unions (TR-Rust.1): generate a small synthetic SDK with an
//! open and a closed discriminated union, then compile and run round-trip
//! assertions against the *real* generated code — proving dispatch,
//! unknown-discriminator retention, and closed-union rejection actually work
//! at runtime, not just that the output compiles.

use std::process::Command;

use gantry_ir::{
    Decl, DeclId, DeclKind, Extensibility, Field, Identifier, ModulePath, Program, StructDecl,
    Type, UnionDecl, UnionVariant,
};

fn ident(s: &str) -> Identifier {
    Identifier::new(s).unwrap()
}

/// A struct carrying the `kind` discriminator plus one own field.
fn variant_struct(program: &mut Program, name: &str, extra: &str) -> DeclId {
    program.add(Decl {
        name: ident(name),
        module: ModulePath(vec![ident("schemas")]),
        api_version: None,
        kind: DeclKind::Struct(StructDecl {
            fields: vec![
                Field {
                    name: ident("kind"),
                    wire_name: "kind".into(),
                    ty: Type::String,
                },
                Field {
                    name: ident(extra),
                    wire_name: extra.to_string(),
                    ty: Type::Optional(Box::new(Type::String)),
                },
            ],
        }),
    })
}

fn union(program: &mut Program, name: &str, ext: Extensibility, variants: &[(&str, DeclId)]) {
    program.add(Decl {
        name: ident(name),
        module: ModulePath(vec![ident("schemas")]),
        api_version: None,
        kind: DeclKind::Union(UnionDecl {
            discriminator: Some("kind".into()),
            variants: variants
                .iter()
                .map(|(value, id)| UnionVariant {
                    discriminator_value: Some((*value).to_string()),
                    ty: Type::Decl(*id),
                })
                .collect(),
            extensibility: ext,
        }),
    });
}

fn synthetic_program() -> Program {
    let mut p = Program::default();
    let dog = variant_struct(&mut p, "Dog", "bark");
    let cat = variant_struct(&mut p, "Cat", "meow");
    let circle = variant_struct(&mut p, "Circle", "radius");
    let square = variant_struct(&mut p, "Square", "side");
    union(
        &mut p,
        "Pet",
        Extensibility::Open,
        &[("dog", dog), ("cat", cat)],
    );
    union(
        &mut p,
        "Shape",
        Extensibility::Closed,
        &[("circle", circle), ("square", square)],
    );
    p
}

const ROUNDTRIP_TEST: &str = r#"
use box_sdk::models::schemas::*;

#[test]
fn open_union_dispatches_roundtrips_and_retains_unknown() {
    // Known discriminator: dispatch + round-trip. The variant's own `kind`
    // field carries the tag; nothing is injected.
    let dog = Pet::Dog(Dog { kind: "dog".to_string(), bark: Some("woof".to_string()) });
    let json = serde_json::to_value(&dog).unwrap();
    assert_eq!(json["kind"], "dog");
    assert_eq!(json["bark"], "woof");
    let back: Pet = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(back, dog);

    // Unknown discriminator is retained verbatim and round-trips (open union).
    let raw = serde_json::json!({ "kind": "fish", "glub": 3 });
    let pet: Pet = serde_json::from_value(raw.clone()).unwrap();
    match &pet {
        Pet::Unknown(v) => assert_eq!(v, &raw),
        other => panic!("expected Unknown, got {other:?}"),
    }
    assert_eq!(serde_json::to_value(&pet).unwrap(), raw);
}

#[test]
fn closed_union_accepts_known_rejects_unknown() {
    let circle: Shape =
        serde_json::from_value(serde_json::json!({ "kind": "circle", "radius": "2" })).unwrap();
    assert!(matches!(circle, Shape::Circle(_)));
    // An unrecognized discriminator is an error (closed union).
    assert!(serde_json::from_value::<Shape>(serde_json::json!({ "kind": "triangle" })).is_err());
}
"#;

#[test]
fn generated_unions_roundtrip() {
    if Command::new("cargo").arg("--version").output().is_err() {
        eprintln!("SKIPPED: cargo toolchain not available; CI runs this gate");
        return;
    }
    let program = Box::leak(Box::new(synthetic_program()));
    let analysis = gantry_sema::analyze(program).expect("synthetic program is well-formed");
    let build = gantry_backend_rust::BuildInfo::new("synthetic");
    let files = gantry_backend_rust::generate(&analysis, &gantry_manifest::rust(), &build);

    let dir = std::env::temp_dir().join(format!("gantry-rust-union-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for file in &files {
        let path = dir.join(&file.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &file.content).unwrap();
    }
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(dir.join("tests/roundtrip.rs"), ROUNDTRIP_TEST).unwrap();

    let output = Command::new("cargo")
        .arg("test")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "generated union round-trip failed (TR-Rust.1, VR-4):\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
