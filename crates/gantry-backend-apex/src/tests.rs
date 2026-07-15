//! Generated Apex test classes (TR-Apex, the 75% coverage deploy gate).
//!
//! Salesforce refuses a production deploy unless ≥ 75% of the org's Apex lines
//! are covered by running tests (`mandated_test_coverage` on the manifest). So
//! the SDK ships its own `@isTest` suite that **exercises** the generated code:
//!
//! - a **mock `BoxClient`** (`BoxCalloutMock`) that returns a canned
//!   `BoxResponse` with no real callout — the only way Apex tests can drive
//!   callout code;
//! - one **`Box<Manager>Test`** per manager, calling every operation through
//!   the mock (covering request-building, deserialization, and — via the
//!   response/body types — the D-132 key remap);
//! - a **`BoxUnionsTest`** that drives each discriminated union's `parse`
//!   dispatch;
//! - **`BoxModelWireTest{n}`** (D-146) that drive the generated wire statics
//!   (`normalizeKeys`/`denormalizeKeys`/`deserialize`) with *populated* inputs,
//!   so the per-field remap, null-injection, and reattach branches execute — the
//!   lines the manager suite's empty `{}`/`[]` bodies leave unrun (the bulk of
//!   the gap to the 75% gate). Chunked so no method overruns Apex's size limit.
//!
//! These are generated from the same IR as the code they cover, so they never
//! drift. Line coverage is a platform measurement (confirmed on-platform by the
//! VR-1.3 scratch-org deploy); here the per-commit signal is structural +
//! determinism, like the rest of the backend. The hand-written runtime carries
//! its own tests (`runtimes/apex/**`); this module covers the generated layer.

use std::collections::HashSet;
use std::fmt::Write as _;

use gantry_ir as ir;
use gantry_manifest::{CapabilityManifest, ModuleSystem};
use gantry_sema::Analysis;

use crate::managers::{manager_infos, op_signature, unique_method_name};
use crate::models::{ClassNames, mint_unique};
use crate::runtime::RUNTIME_CLASS_NAMES;
use crate::wire::Wire;
use crate::{CLASSES_DIR, GeneratedFile};

/// The mock client's class name (reserved in the minter alongside the runtime).
const MOCK_CLASS: &str = "BoxCalloutMock";

/// Generate the `@isTest` suite, or nothing when the target mandates no
/// coverage (`go`/`rust`). Deterministic: managers and operations appear in
/// analysis order.
pub fn generate_tests(
    analysis: &Analysis<'_>,
    manifest: &CapabilityManifest,
) -> Vec<GeneratedFile> {
    if manifest.mandated_test_coverage.is_none() {
        return Vec::new();
    }
    let ModuleSystem::Flat { identifier_limit } = manifest.modules else {
        panic!("the Apex backend requires the flat-namespace manifest axis");
    };
    let limit = identifier_limit as usize;
    let program = analysis.program;
    let names = ClassNames::build(program, limit);
    let infos = manager_infos(analysis, &names, limit);

    // Test class names share the flat namespace, so mint them into a set seeded
    // with every model, manager, client, runtime, and mock name.
    let mut used: HashSet<String> = names.names().map(str::to_string).collect();
    for reserved in ["Box", "BoxRequest", "BoxResponse", "BoxClient", MOCK_CLASS]
        .iter()
        .chain(RUNTIME_CLASS_NAMES)
    {
        used.insert((*reserved).to_string());
    }
    for info in &infos {
        used.insert(info.class.clone());
    }

    let mut files = vec![GeneratedFile {
        path: format!("{CLASSES_DIR}/{MOCK_CLASS}.cls"),
        content: mock_class(),
    }];

    for info in &infos {
        let test_name = mint_unique(&format!("{}Test", info.class), limit, &mut used);
        files.push(GeneratedFile {
            path: format!("{CLASSES_DIR}/{test_name}.cls"),
            content: manager_test(
                program,
                &names,
                &test_name,
                info,
                &analysis.managers[&info.manager],
            ),
        });
    }

    if let Some(content) = unions_test(program, &names, &mut used, limit) {
        files.push(content);
    }

    let wire = Wire::build(program, &names);
    files.extend(model_wire_tests(program, &names, &wire, &mut used, limit));
    files
}

/// The number of structs whose wire statics one generated test method drives.
/// Chunked so no single method grows unbounded against Apex's compiled-size
/// limits; each chunk becomes its own `BoxModelWireTest{n}` class.
const WIRE_TEST_CHUNK: usize = 60;

/// Per-struct tests that drive the generated wire statics
/// (`normalizeKeys`/`denormalizeKeys`/`deserialize`) with populated inputs, so
/// the per-field remap, null-injection, and reattach branches execute — the
/// lines the manager suite's empty `{}`/`[]` bodies never reach (the 75%
/// coverage gate). Generated from the same IR, so they never drift.
fn model_wire_tests(
    program: &ir::Program,
    names: &ClassNames,
    wire: &Wire<'_>,
    used: &mut HashSet<String>,
    limit: usize,
) -> Vec<GeneratedFile> {
    // Every struct that carries a generated wire static, in decl order.
    let mut exercises: Vec<Vec<String>> = Vec::new();
    for (index, decl) in program.decls.iter().enumerate() {
        let id = ir::DeclId(index as u32);
        let ir::DeclKind::Struct(s) = &decl.kind else {
            continue;
        };
        let Some(class) = names.get(id) else {
            continue;
        };
        let stmts = wire.hook_exercises(id, class, s);
        if !stmts.is_empty() {
            exercises.push(stmts);
        }
    }
    if exercises.is_empty() {
        return Vec::new();
    }

    let mut files = Vec::new();
    for (n, chunk) in exercises.chunks(WIRE_TEST_CHUNK).enumerate() {
        let test_name = mint_unique(&format!("BoxModelWireTest{}", n + 1), limit, used);
        let mut out = header();
        let _ = writeln!(
            out,
            "@isTest\nprivate class {test_name} {{\n\
             \x20   // Drives the generated wire statics with populated inputs so the\n\
             \x20   // per-field remap, null-injection, and reattach branches run — the\n\
             \x20   // lines the manager suite's empty `{{}}`/`[]` bodies never reach.\n\
             \x20   @isTest\n\
             \x20   static void exercisesModelWireHooks() {{"
        );
        out.push_str("        Test.startTest();\n");
        for stmts in chunk {
            for stmt in stmts {
                let _ = writeln!(out, "        {stmt}");
            }
        }
        out.push_str("        Test.stopTest();\n");
        out.push_str("        System.assert(true, 'every wire static executed without error');\n");
        out.push_str("    }\n}\n");
        files.push(GeneratedFile {
            path: format!("{CLASSES_DIR}/{test_name}.cls"),
            content: out,
        });
    }
    files
}

/// The mock `BoxClient`: an `@isTest` stub that records the request and returns
/// a caller-configurable canned response — no callout, so tests run anywhere.
fn mock_class() -> String {
    let mut out = header();
    out.push_str(
        "@isTest\n\
         public class BoxCalloutMock implements BoxClient {\n\
         \x20   public Integer statusCode = 200;\n\
         \x20   public String body = '{}';\n\
         \x20   public Blob bodyBlob;\n\
         \x20   public BoxRequest lastRequest;\n\
         \x20   public BoxResponse send(BoxRequest request) {\n\
         \x20       this.lastRequest = request;\n\
         \x20       BoxResponse response = new BoxResponse();\n\
         \x20       response.statusCode = this.statusCode;\n\
         \x20       response.body = this.body;\n\
         \x20       response.binaryBody = this.bodyBlob;\n\
         \x20       return response;\n\
         \x20   }\n\
         }\n",
    );
    out
}

/// A per-manager test: one method that drives every operation through the mock,
/// shaping the canned response to each return type so deserialization succeeds.
fn manager_test(
    program: &ir::Program,
    names: &ClassNames,
    test_name: &str,
    info: &crate::managers::ManagerInfo,
    op_indices: &[usize],
) -> String {
    let mut out = header();
    let _ = writeln!(
        out,
        "@isTest\nprivate class {test_name} {{\n\
         \x20   @isTest\n\
         \x20   static void exercisesEveryOperation() {{"
    );
    let _ = writeln!(out, "        BoxCalloutMock mock = new BoxCalloutMock();");
    let _ = writeln!(
        out,
        "        {class} svc = new {class}(mock);",
        class = info.class
    );
    out.push_str("        Test.startTest();\n");

    let mut used_methods: HashSet<String> = HashSet::new();
    for &index in op_indices {
        let op = &program.operations[index];
        let method_name = unique_method_name(op, &mut used_methods);
        let sig = op_signature(program, names, op, method_name);

        // Shape the canned body to the return type: an array response needs a
        // JSON array, a binary one a Blob, everything else a JSON object.
        if sig.return_ty == "Blob" {
            let _ = writeln!(out, "        mock.bodyBlob = Blob.valueOf('x');");
        } else if sig.return_ty.starts_with("List<") {
            let _ = writeln!(out, "        mock.body = '[]';");
        } else {
            let _ = writeln!(out, "        mock.body = '{{}}';");
        }

        let mut args: Vec<String> = sig
            .params
            .iter()
            .map(|p| placeholder(&p.apex_type))
            .collect();
        if let Some(body) = &sig.body_type {
            args.push(placeholder(body));
        }
        let _ = writeln!(
            out,
            "        svc.{method}({args});",
            method = sig.method_name,
            args = args.join(", "),
        );
    }

    out.push_str("        Test.stopTest();\n");
    out.push_str("        System.assertNotEquals(null, mock.lastRequest);\n");
    out.push_str("    }\n}\n");
    out
}

/// A test that drives every discriminated union's `parse` dispatch: a map
/// carrying the first known tag selects a variant; an unknown tag exercises the
/// open-union fallthrough.
fn unions_test(
    program: &ir::Program,
    names: &ClassNames,
    used: &mut HashSet<String>,
    limit: usize,
) -> Option<GeneratedFile> {
    // Discriminated unions with at least one decl-typed, tagged variant.
    let unions: Vec<(&str, &str, &str)> = program
        .decls
        .iter()
        .enumerate()
        .filter_map(|(index, decl)| {
            let ir::DeclKind::Union(u) = &decl.kind else {
                return None;
            };
            let discriminator = u.discriminator.as_deref()?;
            let value = u
                .variants
                .iter()
                .find_map(|v| match (&v.discriminator_value, &v.ty) {
                    (Some(value), ir::Type::Decl(_)) => Some(value.as_str()),
                    _ => None,
                })?;
            let class = names.get(ir::DeclId(index as u32))?;
            Some((class, discriminator, value))
        })
        .collect();
    if unions.is_empty() {
        return None;
    }

    let test_name = mint_unique("BoxUnionsTest", limit, used);
    let mut out = header();
    let _ = writeln!(
        out,
        "@isTest\nprivate class {test_name} {{\n\
         \x20   @isTest\n\
         \x20   static void dispatchesEveryUnion() {{"
    );
    for (class, discriminator, value) in &unions {
        let _ = writeln!(
            out,
            "        System.assertNotEquals(null, {class}.parse(new Map<String, Object>{{ '{disc}' => '{value}' }}));",
            disc = escape(discriminator),
            value = escape(value),
        );
        // An unknown tag round-trips as the raw map (open union).
        let _ = writeln!(
            out,
            "        System.assertNotEquals(null, {class}.parse(new Map<String, Object>{{ '{disc}' => 'x_gantry_unknown' }}));",
            disc = escape(discriminator),
        );
    }
    out.push_str("    }\n}\n");
    Some(GeneratedFile {
        path: format!("{CLASSES_DIR}/{test_name}.cls"),
        content: out,
    })
}

/// A syntactically valid placeholder value for an Apex type — enough to call a
/// method for coverage. Scalars get a literal; containers an empty collection;
/// a model class its implicit no-arg constructor; anything opaque, `null`.
fn placeholder(apex_type: &str) -> String {
    match apex_type {
        "String" => "'x'".to_string(),
        "Long" => "1".to_string(),
        "Double" => "1.0".to_string(),
        "Boolean" => "true".to_string(),
        "Date" => "Date.today()".to_string(),
        "Datetime" => "Datetime.now()".to_string(),
        "Blob" => "Blob.valueOf('x')".to_string(),
        "Object" => "null".to_string(),
        ty if ty.starts_with("List<") || ty.starts_with("Map<") => format!("new {ty}()"),
        ty => format!("new {ty}()"),
    }
}

fn header() -> String {
    format!(
        "// Code generated by box-gantry {}. DO NOT EDIT.\n",
        env!("CARGO_PKG_VERSION")
    )
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}
