use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Top-level config, merged from user config and optional project override.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Config {
    pub jira: JiraConfig,
    /// Sources in priority order (position = priority, first = highest).
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
    #[serde(default)]
    pub list: ListConfig,
    #[serde(default)]
    pub hide_for_a_day: HideForADayConfig,
    /// Named custom views. Source `view_mode` references a key in this map.
    #[serde(default)]
    pub views: HashMap<String, CustomViewConfig>,
    #[serde(default)]
    pub cache: CacheConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
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

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SourceConfig {
    pub id: String,
    pub display_name: Option<String>,
    pub jql: String,
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
    /// Display value as a formatted datetime using the configured timezone.
    pub datetime: Option<bool>,
    /// Duration row role: "start", "end", or `"jira_value"`.
    /// When a section has both "start" and "end" fields, a read-only duration
    /// row is rendered after that section. `"jira_value"` (float hours) is used
    /// for comparison. Fields with `duration_role` are still editable normally.
    pub duration_role: Option<String>,
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
