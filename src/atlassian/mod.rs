//! The Atlassian Cloud site connection: authentication and OAuth.
//!
//! One Atlassian site serves Jira, Confluence and Agile boards behind a single
//! credential — one API token, or one OAuth app whose scopes span all three.
//! That is why this lives here rather than under [`crate::jira`]: the token is
//! not a Jira token, and [`auth::maybe_refresh`] is shared by the Jira and
//! Confluence clients, which may hold the very same `Arc<RwLock<Auth>>`.
//!
//! [`crate::jira`] keeps everything that is genuinely Jira-the-product —
//! the REST client, JQL, ADF, issue types.

pub mod auth;
pub mod oauth;
