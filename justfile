nixos := `grep -q 'ID=nixos' /etc/os-release 2>/dev/null && echo true || echo false`
wrap  := if nixos == "true" { "nix develop --command " } else { "" }

default:
    @just --list

build:
    {{wrap}}cargo build

check:
    {{wrap}}cargo check

test:
    {{wrap}}cargo test

lint:
    {{wrap}}cargo clippy -- -W clippy::pedantic -W clippy::nursery -W clippy::unwrap_used

lint-fix:
    {{wrap}}cargo clippy --allow-dirty --fix -- -W clippy::pedantic -W clippy::nursery -W clippy::unwrap_used

fmt:
    {{wrap}}cargo fmt

run:
    {{wrap}}cargo run

shell:
    nix develop
