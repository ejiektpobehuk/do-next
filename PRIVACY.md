# Privacy Policy

**do-next** is an open-source command-line tool that runs entirely on your local machine. This policy describes how the application handles your data.

## Data Collection

do-next does **not** collect, transmit, or store any data on external servers operated by the developers. There is no telemetry, analytics, or tracking of any kind.

## Data Stored Locally

do-next stores the following data on your local machine only:

- **Configuration** (`~/.config/do-next/config.json5`) — Jira base URL, project keys, and display preferences.
- **Credentials** — Depending on your chosen authentication method, credentials may be stored in:
  - The system keyring (macOS Keychain, Windows Credential Manager, or Linux Secret Service)
  - A local credentials file (`~/.config/do-next/credentials.json5`)
  - A local OAuth token file (`~/.config/do-next/oauth_tokens.json5`)
- **Cache** (optional) — Issue data may be cached locally if caching is enabled in your configuration.

All locally stored files containing secrets use restrictive file permissions (`600`).

## Third-Party Services

do-next communicates directly with the **Atlassian Jira Cloud API** to read and manage issues on your behalf. If you configure Confluence task sources, it also communicates with the **Atlassian Confluence Cloud API**. When using OAuth authentication, the app also communicates with **Atlassian's authorization servers** (`auth.atlassian.com`) to obtain and refresh access tokens.

No data is sent to any other third party.

## OAuth Scopes

When authenticating via OAuth, do-next always requests the following base permissions:

- `read:jira-work` — Read access to Jira issues, comments, and attachments.
- `write:jira-work` — Write access to create comments, update fields, and manage issues.
- `read:jira-user` — Read access to identify the current user.
- `offline_access` — Allows token refresh without re-authorization.

Additional permissions are requested **only if** your configuration uses the corresponding source kinds:

**Confluence task sources** (read and complete inline tasks, resolve page and space names):

- `read:task:confluence` / `write:task:confluence` — Read and complete inline tasks.
- `read:page:confluence` — Resolve page titles.
- `read:space:confluence` — Resolve space keys.
- `read:content-details:confluence` — Look up the current user.

**Jira board sources** (read boards, sprints, and their issues via the Jira Agile API):

- `read:board-scope:jira-software` / `read:board-scope.admin:jira-software` — Read board issues and configuration.
- `read:sprint:jira-software` — Read sprints and sprint issues.
- `read:project:jira`, `read:issue-details:jira`, `read:jql:jira` — Supporting scopes required by the same endpoints.

You can revoke access at any time from your [Atlassian account settings](https://id.atlassian.com/manage-profile/apps).

## Open Source

do-next is open source. You can inspect exactly what the application does by reviewing the source code at [github.com/ejiektpobehuk/do-next](https://github.com/ejiektpobehuk/do-next).

## Contact

If you have questions about this policy, please open an issue on the [GitHub repository](https://github.com/ejiektpobehuk/do-next/issues).
