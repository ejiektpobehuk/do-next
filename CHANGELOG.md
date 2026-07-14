# Changelog

All notable changes to `do-next` are documented here.

`do-next` uses a date-based pre-release scheme: `v0.0.0-yyyy.mm.dd`.

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
