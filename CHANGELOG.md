# Changelog

All notable changes to `do-next` are documented here.

`do-next` uses a date-based pre-release scheme: `v0.0.0-yyyy.mm.dd`.

## v0.0.0-2026.5.25 — 2026-05-25

Second feature release.
Adds issue search, reusable templates, and a quieter look — plus smaller polish across editing, integrations, and rendering.

### Highlights
- Search overlay with access all issue
- Total and targeted issue reload
- Slack links open straight in the Slack desktop app

### Look & feel
- A calmer, more uniform color palette across the issue list, detail view, and hint bars

### Templates
- Reusable templates for an empty fields
- Preview and usage confirmation
- Multiple templates per field in a team config

### Scrollbars
- Confirmation previews
- Diff and preview tabs in the post-`$EDITOR` confirmation
- Error messages wrap inside the error popup instead of overflowing

### Fixes
- Inline code in ADF output is no longer wrapped in stray marks
- Empty rich-text fields are now recognized from their schema and render correctly

### Technical
- Dependency updates clear all outstanding `cargo audit` warnings
- Nix build switched to crane and to nixpkgs' Rust toolchain
- `clippy` is now available in the dev shell
- Shell completion generation with `do-next completion <shell>`

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
