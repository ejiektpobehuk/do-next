# do-next

> [!IMPORTANT]
> Experimental & unstable! I'm exploring the problem space. Everything might change.

Pick your next Jira task & manage it from the terminal.

<!-- TODO: add a screenshot or demo GIF -->

## Pre-release state

Right now `do-next` is intended for internal use in my teams.

`v0.0.0-yyyy.mm.dd` is the versioning scheme before the release.

Polished experience and documentation are coming closer to the first public release.

---

## Installation

The main way before the release is to [build from source](#development).

The following solutions are supported on a best-effort basis:

### Binaries

Download a compiled binary from [GitHub Releases](https://github.com/ejiektpobehuk/do-next/releases)

### Rust way

Pre-release versions have to be provided explicitly:

```sh
cargo install do-next@0.0.0-yyyy.mm.dd
```

The latest published version: ![Crates.io Version](https://img.shields.io/crates/v/do-next?style=flat-square&label=%40&labelColor=282828&color=3c3836)

### Nix way

This repo provides a flake.
I guess, you know how to configure it on your own ^.~

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

## Confluence tasks

Besides Jira sources, a team config can list Confluence inline tasks (the
checkbox action items on pages) as a source:

```json5
{
  sources: [
    {
      id: "confluence-actions",
      kind: "confluence",
      display_name: "Action items",
      // All filters optional; the default is "my incomplete tasks".
      confluence: {
        spaces: ["ENG"],          // space keys
        pages: ["123456"],        // numeric page IDs
        assignee: "me",           // "me" (default) | "any" | an account id
        status: "incomplete",     // "incomplete" (default) | "complete" | "any"
        due_before: "2026-08-01", // YYYY-MM-DD, inclusive
        due_after: "2026-07-01",
        label: "both",            // list label: "task" (content) | "page" | "both" (default)
      },
      indication: { symbol: "☐", color: "cyan" },
    },
  ],
}
```

Pressing `t` on a Confluence task marks it complete (ticks the checkbox on
the page); `o` opens the page in the browser.

Authentication reuses the Jira connection by default — the same Atlassian
site, email and API token work for Confluence. To point at a different site
or credentials, add a `confluence` block (same optional fields at the user
config or team config level):

```json5
confluence: { base_url: "https://other.atlassian.net", credential_key: "other" }
```

The `DO_NEXT_CONFLUENCE_API_TOKEN` env var overrides the token. With
`auth_method: "oauth"`, re-run `do-next auth` after adding a confluence
source so the token is granted the Confluence scopes; if Atlassian rejects
the combined Jira + Confluence consent, use an API token (`basic`) for
Confluence instead.

---

## Development

Dependencies:

- `just` — *optional* command runner
- `cargo` — the Rust package manager
- msrv — `1.88.0`
- `dbus` — Linux specific dependency

`just` acts as a `cargo` wrapper that overrides some defaults and provides a wrapper for NixOS dev shell.

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
