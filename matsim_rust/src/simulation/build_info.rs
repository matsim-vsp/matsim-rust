use git_version::git_version;

/// Git state of the source tree this crate was compiled from, e.g. `v1.0.0-12-g4f96e9b2-dirty`.
/// `-dirty` marks uncommitted changes to tracked files. Falls back to `v<CARGO_PKG_VERSION>-nogit`
/// when git metadata is unavailable at build time.
pub const GIT_VERSION: &str = git_version!(
    args = [
        "--always",
        "--dirty=-dirty",
        "--tags",
        "--long",
        "--match=v*"
    ],
    cargo_prefix = "v",
    cargo_suffix = "-nogit",
);
