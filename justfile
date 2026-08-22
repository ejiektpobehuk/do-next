# Prefer the flake-pinned toolchain: wrap cargo in `nix develop` when Nix is
# available, unless already inside the dev shell. Opt out: `just --set wrap ""`.
have_nix := `command -v nix >/dev/null 2>&1 && echo true || echo false`
wrap     := if env("IN_NIX_SHELL", "") != "" { "" } else if have_nix == "true" { "nix develop --command " } else { "" }

# List available recipes
default:
    @just --list

# Compile the project, optimized by default; `just build dev` for a debug build
build profile="release":
    {{wrap}}cargo build --profile {{profile}}

# Type-check without producing a binary
check:
    {{wrap}}cargo check

# Run the test suite
test:
    {{wrap}}cargo test

# Run clippy with this project's strict lint set (see [lints] in Cargo.toml)
lint:
    {{wrap}}cargo clippy

# Same lints, but any warning fails — what `nix flake check` enforces
lint-strict:
    {{wrap}}env CARGO_BUILD_WARNINGS=deny cargo clippy

# Auto-apply clippy fixes (allows a dirty working tree)
lint-fix:
    {{wrap}}cargo clippy --allow-dirty --fix

# Format Rust code
fmt:
    {{wrap}}cargo fmt

# Watch files and re-check on change (bacon)
watch:
    {{wrap}}bacon

# Build and run the TUI
run:
    {{wrap}}cargo run

# Update crate dependencies within their semver ranges and commit the new lock file
update:
    {{wrap}}cargo update
    @git diff --quiet -- Cargo.lock || git commit --only Cargo.lock -m "chore: update cargo dependencies"

# Raise dependency version requirements in Cargo.toml; `just upgrade breaking` allows semver-major bumps
upgrade mode="compatible":
    {{wrap}}cargo upgrade {{ if mode == "breaking" { "--incompatible" } else { "" } }}
    {{wrap}}cargo update

# Scan the dependency tree for known security advisories (RustSec)
audit:
    {{wrap}}cargo audit

# Find the minimum supported Rust version; `just msrv verify` checks the manifest instead
msrv command="find":
    {{wrap}}cargo msrv {{command}}

# Enter the Nix dev shell
shell:
    nix develop

# Build the release package via the flake
nix-build:
    nix build

# Format Nix files with the flake's formatter
nix-fmt:
    nix fmt

# Check the flake (evaluates outputs and builds the package)
nix-check:
    nix flake check

# Update flake inputs and commit the new lock file
nix-update:
    nix flake update --commit-lock-file
