# Changelog

All notable changes to `do-next` are documented here.

`do-next` uses a date-based pre-release scheme: `v0.0.0-yyyy.mm.dd`.

## Unreleased

### New features

- Daily standup mode (`kind: "standup"`) — a dedicated tab showing what you did
  since the previous scheduled standup, as a day-by-day timeline across Jira,
  GitLab and Confluence. "Done" means any trace you left: transitions, comments,
  field edits, logged work, issues filed, merge requests opened/merged/closed,
  pages created/edited and tasks ticked off. `<`/`>`/`w`/`d` move the window,
  `y` writes a markdown digest to a file.
- GitLab merge requests as a source kind (`kind: "gitlab"`) — read-only rows
  with approval and CI state, sorted, badged, cached and searched like every
  other source
- `$EDITOR` support for text fields in the new issue form.
- Linked issues support for the new issue form
- Labels support for the new issue form

### Fixes

- An API request answered by an access gateway (Cloudflare Access, Entra ID,
  Okta, Google sign-in) now says so — which gate, and that connecting to the
  VPN is the fix — instead of a JSON decode error carrying a screenful of
  signed redirect URL
- Startup no longer hangs when the config repo's remote is unreachable (VPN
  off): the `git fetch` gets a 5s deadline, the reason is reported as a startup
  warning, and the app opens with the current checkout. Pending updates known
  from the last successful fetch are still reported, marked as stale.
- GitLab requests now time out (10s to connect, 60s total) instead of leaving
  merge-request sources spinning forever against an unreachable instance
- Config timezone offsets with a colon (`"+05:45"`) parsed as whole hours,
  silently dropping the minutes
- HTTP 429 responses now retry once or twice, honouring `Retry-After`

## v0.0.0-2026.7.14 — 2026-07-14

Third feature release.
Adds issue creation, kanban boards, and Confluence tasks — plus a company central config for sharing setup across teams.

### New features

- New issue creation
- Date field type support
- Minimal kanban boards support
- Confluence task support
- Company central config
- Deduplicated Confluence & Jira OAuth entries
- Styled OAuth completion web page

### UX polish

- Keep the form on submission error instead of discarding it
- `q` goes back to the search view without losing state

### Chores / CI

- New nix wrapper & comments for just
- Allow .html files into the build process
- Consolidate multiple bool config fields into a single enum

## v0.0.0-2026.5.25 — 2026-05-25

Second feature release.
Adds issue search, reusable templates, and a quieter look — plus smaller polish across editing, integrations, and rendering.

 ### New features

 - Search MVP
 - Reload MVP
 - Template support, with multiple templates and scrolling/hints in template popups
 - Shell completion
 - Open Slack links directly in the desktop app

 ### UX polish

 - Calmer theme
 - Wrap text in the error popup
 - Scrolling for confirmation previews
 - Aligned post-edit diff and preview

 ### Fixes

 - ADF: detect rich-text fields by schema, not just existing value
 - ADF: drop exclusive marks around inline code

 ### Chores / CI

 - Address cargo audit warnings
 - Flake: switch from oxalica/rust-overlay to nixpkgs' rust
 - Flake: add crane for CI
 - Flake: add clippy to dev shell
 - Add .gitignore for CI

## v0.0.0-2026.4.8 — 2026-04-08

First feature release since the initial public pre-release.
Brings Jira Cloud support, team config files, and a much richer issue editing experience.

### Jira Cloud support
- Added Cloud API v3
- Dropped Data Center API v2
- Added limited OAuth authentication — you have to register your own Atlassian app

### Customizable views
- Replaced hardcoded Incident / Postmortem / Review views with a fully customizable view system
- Team configuration is now split into separate config files

### Issue editing improvements
- Comments management popup
- Attachments management popup
- Post-`$EDITOR` confirmation popup with prerender and diff tabs

### Markdown & rendering
- ADF (Atlassian Document Format) ↔ Markdown conversion for better editor UX
- Minimal Markdown rendering in the terminal
