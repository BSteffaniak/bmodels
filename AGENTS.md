# bmodels Agent Guidelines

bmodels watches model registries for new and changed AI models and remembers what you've seen.
See `docs/PLAN.md` for the architecture.

## Build/Test Commands

- **Rust build**: `cargo build`
- **Rust test**: `cargo nextest run --no-fail-fast` (or `cargo test`), `cargo test -p <package>` (single package)
- **Rust lint**: `cargo clippy --all-targets -- -D warnings` (zero warnings is REQUIRED)
- **Unused deps**: `cargo machete --with-metadata` (zero findings is REQUIRED)
- **Format**: `cargo fmt` (Rust) for ALL packages in the workspace; `taplo fmt` for TOML
- **Install globally**: `cargo install --path packages/bmodels`

## Code Style Guidelines

### Rust Patterns

- **Collections**: Always use `BTreeMap`/`BTreeSet`, never `HashMap`/`HashSet`
- **Dependencies**: Use `workspace = true` in package manifests, never inline versions
- **Upstream sibling crates**: bmux and switchy are path dependencies on the sibling checkouts
  (`../bmux`, `../MoosicBox`) in the workspace `Cargo.toml` for now; they will move to git/crates.io
  later. If a sibling crate pulls another switchy/moosicbox crate from crates.io, add a
  `[patch.crates-io]` entry pointing at the sibling path so they unify.
- **New Dependencies**: When adding a new dependency:
    - Add to workspace `Cargo.toml` with `default-features = false`
    - Specify full version including patch (e.g., `"0.4.28"` not `"0.4"`)
    - Verify you're using the LATEST stable version from crates.io
    - In package `Cargo.toml`, use `workspace = true` and opt-in to specific features only
- **Clippy**: Required in every crate root:
    ```rust
    #![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
    #![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
    #![allow(clippy::multiple_crate_versions)]
    ```
- Fix clippy root causes. No crate-level `allow`s beyond the header; item-level `#[allow]` only
  with a comment explaining why.
- No wildcard imports outside `#[cfg(test)] mod tests { use super::*; }`.
- **Unsafe**: Not expected anywhere in this workspace.
- **Databases**: All database access goes through `switchy_database` /
  `switchy_database_connection` (SQLite via `sqlite-rusqlite`), never raw `rusqlite`.
- **Configuration**: All configuration loading goes through `bmodels_config`, built on
  `bmux_config_composition` (layers, search policy, TOML codec, source tracking). Never hand-roll
  config file discovery, merging, or parsing with `toml`/`serde` directly; upstream generic gaps
  to bmux instead.
- **HTTP**: All HTTP goes through `switchy_http`. Upstream generic gaps to MoosicBox rather than
  adding another HTTP client.
- **Secrets**: API keys are only ever read from env vars or commands named in config; never log,
  persist, or serialize them.
- **Rustdoc Error docs**: Use asterisks (*) for bullet points, document all error conditions
- **Must use**: Add `#[must_use]` to constructors and getters that return types OTHER THAN Result or Option

### Package Organization

- **Naming**: All packages use underscore naming (`bmodels_store`); the binary package is `bmodels`
- **Features**: Always include `fail-on-warnings = []` feature and propagate it to internal deps
- **Serde**: Use `SCREAMING_SNAKE_CASE` for enum value renames; struct keys are `snake_case`

### Architecture Boundaries

- `bmodels_model` is the shared contract. Keep it dependency-light and free of I/O.
- `bmodels_sources` must not depend on `bmodels_store`; `bmodels_engine` is the only crate that
  combines fetching and persistence.
- Source adapters are tested against recorded JSON fixtures, never live endpoints, in CI.
- The TUI builds on `bmux_tui`, `bmux_tui_components`, and `bmux_tui_runtime`. Prefer upstreaming
  missing generic components to bmux over local reimplementation.

### Documentation

- Document all public APIs with comprehensive error information
- Include examples for complex functions
