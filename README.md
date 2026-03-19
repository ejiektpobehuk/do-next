# do-next

> [!IMPORTANT]
> Experimental & unstable! I'm exploring the problem space. Everything might change.

Pick your next Jira task & manage it from the terminal.

<!-- TODO: add a screenshot or demo GIF -->

## Pre-release state

Right now `do-next` is intended for internal use in my teams.

`v0.0.0-yyyy-mm-dd` is the versioning scheme before the release.

Polished experience and documentation are coming closer to the first public release.

---

## Installation

The main way before the release is to [build from source](#development).

Following solutions are supported at the best effort:

### Binaries

Download a compiled binary from [GitHub Releases](https://github.com/ejiektpobehuk/do-next/releases)

### Rust way

```sh
cargo install do-next
```

### Nix way

This repo provides a flake.
I guess, you know how to configure in on your own ^.~

```sh
nix run github:ejiektpobehuk/do-next
```

## Runtime dependencies

macOS and Windows have no extra dependencies.

Linux depends on:
- `xdg-utils` to open Issues in a browser
- `dbus` & secret service provider for optional keyring secret management

---

## Quick start

`do-next` has a built-in onboarding.

`do-next auth` to reconfigure authentication only.

---

## Development

Dependencies:

- `just` — *optional* command runner
- `cargo` — the Rust package manager
- msrv — `1.88.0`
- `dbus` — Linux specific dependency

`just` acts as a `cargo` wrapper that overwrites some defaults and provides a wrapper for NixOS dev shell.

Run `just` to list all available commands:

```
just
Available recipes:
    build
    check
    default
    lint
    lint-fix
    run
    shell
    test
```

### NixOS

libdbus is a runtime and a build dependency.
You'll need the dev shell to handle it properly.
`just` handles calling the dev shell if it detects that it's running in a NixOS environment.

### Non-NixOS Linux

The `sync-secret-service` keyring backend requires the dbus development headers:

**Arch Linux**
```sh
sudo pacman -S dbus pkgconf
```

**Debian/Ubuntu**
```sh
sudo apt install libdbus-1-dev pkg-config
```

**Fedora**
```sh
sudo dnf install dbus-devel pkgconf-pkg-config
```

### macOS / Windows

```sh
just build
```

or

```sh
cargo build
```

No extra system dependencies required.

---

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
