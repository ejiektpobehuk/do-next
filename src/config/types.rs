use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Top-level user config (personal settings + team references).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Config {
    #[serde(default)]
    pub jira: JiraConfig,
    /// Confluence connection settings. Anything unset falls back to the
    /// effective Jira config (same Atlassian site: one API token covers both).
    #[serde(default)]
    pub confluence: Option<ConfluenceConfig>,
    #[serde(default)]
    pub cache: CacheConfig,
    /// Team references. Onboarding creates at least one ("personal").
    #[serde(default)]
    pub teams: Vec<TeamRef>,
    /// Open `*.slack.com` links in the Slack desktop app instead of the browser.
    /// Defaults to `true`. Can be overridden per-team or per-field.
    pub open_slack_in_app: Option<bool>,
    /// Slack workspace (team) ID (e.g. "T0123ABCDEF"). Required for deep links.
    pub slack_team_id: Option<String>,
}

/// A reference to a team config directory.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TeamRef {
    /// Short identifier used for display (tab label) and hidden-state namespacing.
    pub id: String,
    /// Path to the directory containing the team config file.
    pub path: String,
    /// Config file name inside `path` (default: "do-next.json5").
    pub file: Option<String>,
}

/// Partial Jira overrides for team configs. All fields optional — only set fields
/// override the user's default Jira config.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct TeamJiraOverride {
    pub base_url: Option<String>,
    pub default_project: Option<String>,
    pub email: Option<String>,
    pub credential_command: Option<String>,
    pub credential_store: Option<String>,
    pub credential_key: Option<String>,
    pub auth_method: Option<String>,
    pub oauth_client_id: Option<String>,
    pub oauth_client_secret: Option<String>,
}

/// Team-level config: shareable across team members via a git repo.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct TeamConfig {
    /// Optional Jira overrides. If absent, inherits the user's default `jira`.
    #[serde(default)]
    pub jira: Option<TeamJiraOverride>,
    /// Sources in priority order (position = priority, first = highest).
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
    #[serde(default)]
    pub list: ListConfig,
    #[serde(default)]
    pub hide_for_a_day: HideForADayConfig,
    /// Optional Confluence connection overrides for this team. Unset fields
    /// fall back to the user's `confluence` config, then the effective Jira
    /// config.
    #[serde(default)]
    pub confluence: Option<ConfluenceConfig>,
    /// Named custom views. Source `view_mode` references a key in this map.
    #[serde(default)]
    pub views: HashMap<String, CustomViewConfig>,
    /// Open `*.slack.com` links in the Slack desktop app instead of the browser.
    /// Overrides the global setting. Defaults to the global value (or `true`).
    pub open_slack_in_app: Option<bool>,
    /// Slack workspace (team) ID. Overrides the global setting.
    pub slack_team_id: Option<String>,
}

/// A fully resolved team: team ref + loaded config + effective Jira config.
#[derive(Debug, Clone)]
pub struct ResolvedTeam {
    pub id: String,
    pub path: String,
    pub config: TeamConfig,
    /// Effective Jira config (team override merged on top of user default).
    pub jira: JiraConfig,
    /// Effective Confluence connection config (team confluence override →
    /// user confluence → effective Jira). Expressed as a `JiraConfig` so the
    /// same credential resolution applies.
    pub confluence: JiraConfig,
    /// Effective setting: open `*.slack.com` links in the Slack desktop app.
    pub open_slack_in_app: bool,
    /// Effective Slack workspace (team) ID for deep links.
    pub slack_team_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Default)]
pub struct JiraConfig {
    pub base_url: String,
    pub default_project: String,
    /// Jira account email used for API authentication.
    pub email: Option<String>,
    /// Shell command whose stdout yields a Jira API token.
    pub credential_command: Option<String>,
    /// Use OS keyring for credentials.
    pub credential_store: Option<String>,
    /// Key label for keyring lookup (defaults to `base_url`).
    pub credential_key: Option<String>,
    /// Authentication method: "basic" (default) or "oauth".
    pub auth_method: Option<String>,
    /// OAuth client ID from your Atlassian Developer Console app.
    pub oauth_client_id: Option<String>,
    /// OAuth client secret from your Atlassian Developer Console app.
    pub oauth_client_secret: Option<String>,
}

/// Which backend a source fetches from. Absent in config = Jira.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    #[default]
    Jira,
    Confluence,
}

/// What identifies a Confluence task in the list: its content, the page it
/// lives on, or both ("content · page").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfluenceLabel {
    Task,
    Page,
    #[default]
    Both,
}

/// Per-source Confluence inline-task filters.
/// An absent block means "my incomplete tasks".
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ConfluenceFilters {
    /// Space keys (e.g. "ENG"); resolved to numeric space IDs at fetch time.
    #[serde(default)]
    pub spaces: Vec<String>,
    /// Numeric page IDs (kept as strings for JSON5 friendliness).
    #[serde(default)]
    pub pages: Vec<String>,
    /// "me" (default) | "any" | an explicit Atlassian account id.
    pub assignee: Option<String>,
    /// "incomplete" (default) | "complete" | "any".
    pub status: Option<String>,
    /// Calendar dates "YYYY-MM-DD", inclusive bounds on the task due date.
    pub due_before: Option<String>,
    pub due_after: Option<String>,
    /// Include checkbox items with empty text. Defaults to false.
    #[serde(default)]
    pub include_blank: bool,
    /// How a task is labeled in the list: "task" (its content), "page"
    /// (the page it lives on) or "both" (default, "content · page").
    #[serde(default)]
    pub label: ConfluenceLabel,
}

/// Partial Confluence connection overrides. All fields optional — anything
/// unset falls back to the effective Jira config (same site, same token).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ConfluenceConfig {
    pub base_url: Option<String>,
    pub email: Option<String>,
    pub credential_command: Option<String>,
    pub credential_store: Option<String>,
    pub credential_key: Option<String>,
    pub auth_method: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SourceConfig {
    pub id: String,
    pub display_name: Option<String>,
    /// Which backend this source fetches from. Defaults to Jira.
    #[serde(default)]
    pub kind: SourceKind,
    #[serde(default)]
    pub jql: String,
    /// Confluence task filters; only meaningful when `kind` is `confluence`.
    #[serde(default)]
    pub confluence: Option<ConfluenceFilters>,
    /// Project key for wrong-project detection (e.g. incidents).
    pub expected_project: Option<String>,
    /// Sort order within source: "updated", "created", "priority".
    pub order_within: Option<String>,
    /// Whether "Hide for a day" is available for this source.
    #[serde(default)]
    pub allow_hide_for_a_day: bool,
    /// Custom view ID (key in `config.views`). Absent = Default view.
    pub view_mode: Option<String>,
    /// Display indication (symbol + color). Falls back to `list.default_indication`.
    pub indication: Option<SourceIndication>,
    /// If present, one Jira fetch per subsource using combined JQL.
    /// Note: parent `jql` must not contain ORDER BY when subsources are defined.
    #[serde(default)]
    pub subsources: Vec<SubsourceConfig>,
    /// Source-level badges: "stale" | "assignee"
    #[serde(default)]
    pub badges: Vec<String>,
}

impl SourceConfig {
    pub fn display_name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SubsourceConfig {
    pub jql_filter: String,
    pub badge: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ListConfig {
    pub default_indication: Option<SourceIndication>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SourceIndication {
    pub symbol: String,
    pub color: String,
    pub separator_text: Option<String>,
}

impl Default for SourceIndication {
    fn default() -> Self {
        Self {
            symbol: "•".into(),
            color: "default".into(),
            separator_text: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct HideForADayConfig {
    pub duration_hours: Option<u32>,
    pub duration_days: Option<u32>,
    #[serde(default)]
    pub suggested_solutions: Vec<SuggestedSolution>,
}

impl HideForADayConfig {
    pub const fn duration_hours(&self) -> u32 {
        if let Some(h) = self.duration_hours {
            return h;
        }
        if let Some(d) = self.duration_days {
            return d * 24;
        }
        24
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SuggestedSolution {
    pub label: String,
    pub link: Option<String>,
    pub copy_template: Option<String>,
}

/// Configuration for a single field in a custom view section.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct CustomViewFieldConfig {
    pub field_id: String,
    /// Override for the display name; if absent, name is fetched from Jira editmeta.
    pub name: Option<String>,
    /// Hint text shown in the hint bar when editing this field.
    pub hint: Option<String>,
    /// View-only: don't open editing on Enter. For URL values, Enter opens the link in a browser.
    pub readonly: Option<bool>,
    /// Always open $EDITOR regardless of field type.
    pub use_editor: Option<bool>,
    /// Value type. `"date"`: calendar date (`yyyy-MM-dd`, e.g. Due Date),
    /// displayed without a time part and edited with a date-only picker.
    /// `"datetime"`: displayed as a formatted datetime using the configured
    /// timezone; editing opens the full datetime picker.
    pub r#type: Option<FieldType>,
    /// Deprecated: use `type: "datetime"` instead.
    pub datetime: Option<bool>,
    /// Deprecated: use `type: "date"` instead.
    pub date: Option<bool>,
    /// Duration row role: "start", "end", or `"jira_value"`.
    /// When a section has both "start" and "end" fields, a read-only duration
    /// row is rendered after that section. `"jira_value"` (float hours) is used
    /// for comparison. Fields with `duration_role` are still editable normally.
    pub duration_role: Option<String>,
    /// How to open this field's URL: `"browser"` or `"slack"`.
    /// Overrides the team/global `open_slack_in_app` setting for this field.
    pub open_with: Option<String>,
    /// Shortcut for a single unnamed template (relative to the team config
    /// directory). Mutually exclusive with `templates`.
    pub template: Option<String>,
    /// Named template files (relative to the team config directory).
    /// When the field is empty and an editor is opened, the user is offered
    /// to pre-load a template. Each entry has a `name` (shown in the picker)
    /// and a `path` to the markdown file.
    pub templates: Option<Vec<TemplateEntry>>,
}

/// Field value type, set via `type` in config (the deprecated `date`/`datetime`
/// boolean flags map onto the same variants).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    /// Calendar date only (`yyyy-MM-dd`).
    Date,
    /// Date with time, timezone-aware.
    DateTime,
}

impl CustomViewFieldConfig {
    /// Resolves the field's kind: `type` when set, otherwise the deprecated
    /// `date`/`datetime` flags. Caller should have validated that the
    /// combination is consistent.
    pub fn effective_type(&self) -> Option<FieldType> {
        if self.r#type.is_some() {
            return self.r#type;
        }
        if self.date == Some(true) {
            Some(FieldType::Date)
        } else if self.datetime == Some(true) {
            Some(FieldType::DateTime)
        } else {
            None
        }
    }

    /// True when the deprecated `date`/`datetime` boolean flags are present.
    pub const fn uses_legacy_date_flags(&self) -> bool {
        self.date.is_some() || self.datetime.is_some()
    }

    /// Returns the unified list of templates from `template` and `templates`.
    /// Caller should have validated that they aren't both set.
    pub fn effective_templates(&self) -> Vec<TemplateEntry> {
        if let Some(path) = &self.template {
            return vec![TemplateEntry {
                name: String::new(),
                path: path.clone(),
            }];
        }
        self.templates.clone().unwrap_or_default()
    }
}

/// A named template file for a field.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TemplateEntry {
    pub name: String,
    pub path: String,
}

/// A section within a custom view.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct CustomViewSectionConfig {
    pub title: String,
    /// Optional subtitle shown below the section separator.
    pub description: Option<String>,
    pub fields: Vec<CustomViewFieldConfig>,
}

/// Configuration for a named custom view.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct CustomViewConfig {
    /// Display timezone, e.g. "+03" or "-05". Defaults to system local timezone.
    pub timezone: Option<String>,
    pub sections: Vec<CustomViewSectionConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct CacheConfig {
    #[serde(default)]
    pub enabled: bool,
    pub max_age_seconds: Option<u64>,
    pub path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_type_is_none_by_default() {
        let field = CustomViewFieldConfig::default();
        assert_eq!(field.effective_type(), None);
    }

    #[test]
    fn effective_type_from_type() {
        let field = CustomViewFieldConfig {
            r#type: Some(FieldType::Date),
            ..Default::default()
        };
        assert_eq!(field.effective_type(), Some(FieldType::Date));

        let field = CustomViewFieldConfig {
            r#type: Some(FieldType::DateTime),
            ..Default::default()
        };
        assert_eq!(field.effective_type(), Some(FieldType::DateTime));
    }

    #[test]
    fn type_deserializes_from_lowercase_names() {
        let field: CustomViewFieldConfig =
            json5::from_str(r#"{ field_id: "duedate", type: "date" }"#).expect("valid config");
        assert_eq!(field.effective_type(), Some(FieldType::Date));

        let field: CustomViewFieldConfig =
            json5::from_str(r#"{ field_id: "created", type: "datetime" }"#).expect("valid config");
        assert_eq!(field.effective_type(), Some(FieldType::DateTime));
    }

    #[test]
    fn effective_type_from_legacy_date_flag() {
        let field = CustomViewFieldConfig {
            date: Some(true),
            ..Default::default()
        };
        assert_eq!(field.effective_type(), Some(FieldType::Date));
    }

    #[test]
    fn effective_type_from_legacy_datetime_flag() {
        let field = CustomViewFieldConfig {
            datetime: Some(true),
            ..Default::default()
        };
        assert_eq!(field.effective_type(), Some(FieldType::DateTime));
    }

    #[test]
    fn type_wins_over_legacy_flags() {
        let field = CustomViewFieldConfig {
            r#type: Some(FieldType::DateTime),
            date: Some(true),
            ..Default::default()
        };
        assert_eq!(field.effective_type(), Some(FieldType::DateTime));
    }

    #[test]
    fn effective_type_ignores_explicit_false() {
        let field = CustomViewFieldConfig {
            date: Some(false),
            datetime: Some(false),
            ..Default::default()
        };
        assert_eq!(field.effective_type(), None);
    }
}
