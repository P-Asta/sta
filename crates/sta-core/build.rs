//! Build script: the release gate of the debug-only MCP test surface (docs/TESTING.md, lock 1).
//!
//! `lib.rs`'s `compile_error!` keys off `debug_assertions`, and a release profile can turn those back
//! on (`CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true`, a "release with assertions" variant) — the
//! guard then passes and the whole surface compiles into an **optimized** binary. Cargo tells a build
//! script the profile itself, so the rule is enforced here where nothing can argue with it.
//!
//! Every crate that serves the surface (`sta`, `sta-mcp`) enables `sta-core/test-hooks`, so this one
//! script covers all of them. `tools/check-release-clean.mjs` remains the second net, on the binary.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let release = matches!(std::env::var("PROFILE").as_deref(), Ok("release"));
    assert!(
        !(release && std::env::var_os("CARGO_FEATURE_TEST_HOOKS").is_some()),
        "the `test-hooks` feature is debug-only: it must never be built into a release binary (docs/TESTING.md lock 1)"
    );
}
