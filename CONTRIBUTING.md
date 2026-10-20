# Contributing

Issues and pull requests are welcome.

- `cargo test --workspace` and `cargo clippy --workspace --all-targets` should pass.
- The examples build on their own: `cd examples/echo-server && cargo test`, `cd examples/bevy-rooms && cargo build`.
- `crates/partii-client/src/generated.rs` is generated from partii's public OpenAPI spec (https://partii.games/docs/openapi.yaml); don't edit it by hand. If an operation is wrong, open an issue.
- Breaking changes to public types need a minor version bump before 1.0, and a CHANGELOG entry. `scripts/release.sh check` runs `cargo-semver-checks`.
- By contributing you agree your contribution is dual-licensed under MIT OR Apache-2.0.
