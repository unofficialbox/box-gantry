//! The vendored real Box specs must always ingest cleanly — the earliest
//! form of the "generate the real spec" primary CI signal (VR-1.1 lineage).

use std::collections::HashSet;
use std::path::PathBuf;

use gantry_spec::SpecSet;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/specs")
        .join(name)
}

#[test]
fn the_full_real_spec_set_ingests() {
    let set = SpecSet::load(&[
        fixture("openapi.json"),
        fixture("openapi-v2025.0.json"),
        fixture("openapi-v2026.0.json"),
    ])
    .expect("the vendored Box specs must ingest without errors");

    let versions: Vec<&str> = set
        .documents
        .iter()
        .map(|d| d.api_version.as_str())
        .collect();
    assert_eq!(versions, ["2024.0", "2025.0", "2026.0"]);

    let base = &set.documents[0];
    // Counts as of the vendored snapshot (see fixtures/specs/README.md).
    // If a spec refresh changes them, updating these numbers is the
    // deliberate, reviewed act the determinism rules want (FR-6.2, VR-6).
    assert_eq!(base.operations.len(), 297);
    assert_eq!(base.schemas.len(), 310);
    assert_eq!(base.managers().len(), 73);

    assert_eq!(set.documents[1].operations.len(), 37);
    assert_eq!(set.documents[2].operations.len(), 5);

    // Every operation everywhere is grouped and identified.
    for doc in &set.documents {
        for op in &doc.operations {
            assert!(!op.id.is_empty() && !op.manager.is_empty());
        }
    }

    // The whole set lowers into typed IR with zero errors; the counts are
    // pinned so growth and free-form holes only change deliberately (NF-1,
    // VR-6 lineage).
    let lowering = gantry_spec::lower(&set).expect("the vendored Box specs must lower");
    // After structural dedupe (D-127) identical inline shapes collapse, the
    // version merge (D-190) collapses 21 same-named cross-version schemas (16
    // structs + 5 enums) into one superset each, stripping the auto-set
    // `box-version` header (D-191) drops its 2 inline version enums,
    // recognizing the nullable-`$ref` `oneOf`/`anyOf` idiom (D-195) resolves
    // 9 fields straight to their referenced type instead of synthesizing an
    // opaque structural union for each, and the v2026.0 Box Query / Query
    // Insights release adds 16 new decls. D-196 gives 3 of those — including
    // `QueryResultEntry`'s open metadata bag — a real extra-fields
    // representation instead of a silent drop. The same silent drop applied to
    // `GenericSource`/`AiExtractResponse`, which D-195's fix also caught:
    // 900 → 879 → 877 → 868 → 884 decls (486 synthesized). The 2026-10 refresh
    // adds upload-session planning (4 structs) and the Japanese ZIP sign-input
    // validation (1 struct + its synthesized type enum): → 890.
    assert_eq!(lowering.program.decls.len(), 890);
    let stats = &lowering.stats;
    assert_eq!(
        (stats.structs, stats.unions, stats.discriminated_unions),
        (610, 33, 23)
    );
    assert_eq!((stats.enums, stats.aliases), (245, 2));
    assert_eq!(stats.synthesized, 487);
    assert_eq!(stats.json_value_sites, 19);

    // Operations: every one lowered, with classified success shapes.
    assert_eq!(lowering.program.operations.len(), 339);
    assert_eq!(stats.operations, 339);
    assert_eq!(
        (
            stats.empty_responses,
            stats.binary_responses,
            stats.text_responses,
            stats.redirect_responses
        ),
        (56, 4, 1, 0)
    );
    // Nineteen `#variation` ids became structured variations (D-104).
    let variations = lowering
        .program
        .operations
        .iter()
        .filter(|op| op.variation.is_some())
        .count();
    assert_eq!(variations, 19);
    // The base-URL quirk (G-2): non-default servers map to the closed set.
    let non_default = lowering
        .program
        .operations
        .iter()
        .filter(|op| op.base_url != gantry_ir::BaseUrl::Api)
        .count();
    // 15 since the 2026-10 refresh: upload-session planning rides the
    // upload-session server like the rest of the chunked-upload endpoints.
    assert_eq!(non_default, 15);

    // The semantic pass verifies the whole real program (FR-3): every
    // reference bound, every type well-formed, identities unique.
    let analysis = gantry_sema::analyze(&lowering.program)
        .expect("the real program must pass semantic analysis");
    assert_eq!(analysis.managers.len(), 86);
    let indexed: usize = analysis.managers.values().map(Vec::len).sum();
    assert_eq!(indexed, 339);

    // Every declaration carries a reachability entry, and the split between
    // sole-owned (goes to a per-manager file, D-201), shared/orphaned (stays
    // in the catch-all), and true orphans is pinned so it only moves
    // deliberately with the spec (VR-6 lineage), exactly like the decl and
    // operation counts above.
    assert_eq!(analysis.decl_managers.len(), lowering.program.decls.len());
    let sole = (0..lowering.program.decls.len())
        .filter(|&i| analysis.sole_manager(i).is_some())
        .count();
    let orphan = analysis
        .decl_managers
        .iter()
        .filter(|owners| owners.is_empty())
        .count();
    // The 2026-10 refresh: its 6 new decls are each sole-owned, and
    // `CollaborationItem` now references `WebLinkMini`, adopting a former
    // orphan into the shared set.
    assert_eq!((sole, orphan), (661, 36));
}

/// Method names never fall through to a meaningless numeric collision suffix,
/// and the curated endpoints (D-194) carry their intended names.
#[test]
fn method_names_are_curated_not_collision_suffixed() {
    let set = SpecSet::load(&[
        fixture("openapi.json"),
        fixture("openapi-v2025.0.json"),
        fixture("openapi-v2026.0.json"),
    ])
    .unwrap();
    let lowering = gantry_spec::lower(&set).unwrap();

    let named = |manager: &str, name: &str| {
        lowering
            .program
            .operations
            .iter()
            .any(|op| op.manager.as_str() == manager && op.name.as_str() == name)
    };
    // The two /files/content uploads used to collide on `createFileContent`
    // (one losing to `createFileContent2`); the transfer endpoint leaked Box's
    // literal root-folder `0`.
    assert!(named("uploads", "uploadFile"), "upload-new-file name");
    assert!(named("uploads", "uploadFileVersion"), "upload-version name");
    assert!(named("transfer", "transferFolders"), "transfer name");
    assert!(named("search", "queryByMetadata"), "metadata-query name");
    assert!(
        !lowering
            .program
            .operations
            .iter()
            .any(|op| op.name.as_str().eq_ignore_ascii_case("createFileContent2")),
        "the createFileContent collision must be gone"
    );

    // No method name ends in a bare numeric collision suffix. `oauth2` is a
    // legitimate protocol name (`/oauth2/revoke`), not a `_2` disambiguator, so
    // the guard targets the `<name>2`/`<name>0` shape a real name never takes.
    for op in &lowering.program.operations {
        let name = op.name.as_str();
        let ends_digit = name.ends_with(|c: char| c.is_ascii_digit());
        let is_oauth = name.to_ascii_lowercase().contains("oauth2");
        assert!(
            !ends_digit || is_oauth,
            "operation {}/{} has a collision-suffixed name",
            op.manager.as_str(),
            name
        );
    }
}

/// Synthesized type names fall back to real structure — the deeper ancestor,
/// then the name before the duplicate-word seam collapse — before a numeric
/// collision suffix, the type-name counterpart of the method-name guard above.
#[test]
fn type_names_fall_back_to_structure_not_collision_suffixed() {
    let set = SpecSet::load(&[
        fixture("openapi.json"),
        fixture("openapi-v2025.0.json"),
        fixture("openapi-v2026.0.json"),
    ])
    .unwrap();
    let lowering = gantry_spec::lower(&set).unwrap();

    let named_at = |location: &str| {
        lowering
            .synthesis_log
            .iter()
            .find(|(at, _)| at == location)
            .map(|(_, name)| name.as_str())
            .unwrap_or_else(|| panic!("nothing synthesized at {location}"))
    };
    // A field named `<owner>_type` beside the owner's own `type` used to lose
    // the seam-collapsed `<Owner>Type` to its sibling and fall to `…Type2`; a
    // field named after its owner collapsed onto the owner itself.
    for (location, name) in [
        ("components.schemas.Group.properties.type", "GroupType"),
        (
            "components.schemas.Group.properties.group_type",
            "GroupGroupType",
        ),
        (
            "components.schemas.Collection.properties.collection_type",
            "CollectionCollectionType",
        ),
        (
            "components.schemas.RetentionPolicy.properties.policy_type",
            "RetentionPolicyPolicyType",
        ),
        (
            "components.schemas.KeywordSkillCard.properties.skill_card_type",
            "KeywordSkillCardSkillCardType",
        ),
        (
            "components.schemas.CollaboratorVariable.properties.variable_type",
            "CollaboratorVariableVariableType",
        ),
        (
            "components.schemas.AutomateWorkflowAction.properties.action_type",
            "AutomateWorkflowActionActionType",
        ),
        (
            "components.schemas.Watermark.properties.watermark",
            "WatermarkWatermark",
        ),
        (
            "components.schemas.ResourceScope.properties.scope",
            "ResourceScopeScope",
        ),
        (
            "components.schemas.ShieldInformationBarrierReportDetails.properties.details",
            "ShieldInformationBarrierReportDetailsDetails",
        ),
    ] {
        assert_eq!(named_at(location), name, "name synthesized at {location}");
    }

    // The collision signature: `<Name><digits>` where `<Name>` itself is
    // also taken. A structural name that merely ends in a digit (`…Variant0`,
    // an `OAuth2` segment) has no such twin, so it doesn't trip this.
    // `ClassificationTemplate2` is a different pattern — `$template` names a
    // whole other component (`ClassificationTemplate`), with no collapse to
    // undo and no deeper ancestor — and stays the one known numeral.
    let names: HashSet<&str> = lowering
        .synthesis_log
        .iter()
        .map(|(_, name)| name.as_str())
        .collect();
    let mut suffixed: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| {
            let stem = name.trim_end_matches(|c: char| c.is_ascii_digit());
            stem.len() < name.len() && names.contains(stem)
        })
        .collect();
    suffixed.sort_unstable();
    assert_eq!(
        suffixed,
        ["ClassificationTemplate2"],
        "a synthesized type name fell through to a numeric collision suffix"
    );
}

#[test]
fn the_spec_fingerprint_is_deterministic_and_order_sensitive() {
    let load = |files: &[PathBuf]| SpecSet::load(files).unwrap().fingerprint();
    let base_then_2025 = [fixture("openapi.json"), fixture("openapi-v2025.0.json")];
    let reversed = [fixture("openapi-v2025.0.json"), fixture("openapi.json")];

    // Deterministic: same inputs in the same order → same fingerprint (NF-7).
    assert_eq!(load(&base_then_2025), load(&base_then_2025));
    // Sixteen lowercase hex digits.
    let fp = load(&base_then_2025);
    assert_eq!(fp.len(), 16);
    assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));

    // Load order changes the fingerprint (the set is ordered — base first).
    assert_ne!(load(&base_then_2025), load(&reversed));
    // Adding a document changes it.
    assert_ne!(load(&base_then_2025), load(&[fixture("openapi.json")]));
}
