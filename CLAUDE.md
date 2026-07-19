# box-gantry

Project guidance for Claude Code. The full guide — build/verify commands,
conventions, and where things live — is shared with other assistants in
`AGENTS.md`:

@AGENTS.md

Quick reminders:

- Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  and `cargo test --workspace` before finishing a change — that's the CI gate.
- Keep generated output deterministic and dependency-light; put language behavior
  in the capability manifest, not in `if language == …` branches.
- Record non-obvious decisions as `D-###` entries in `DECISIONS.md`.
