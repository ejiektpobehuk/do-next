//! Grafana `OnCall` API response types and the pure on-duty decision logic.

use serde::Deserialize;

/// The user a personal `OnCall` API token belongs to
/// (`GET /api/v1/users/current/`).
#[derive(Debug, Clone, Deserialize)]
pub struct OnCallUser {
    pub id: String,
    pub email: Option<String>,
    pub username: Option<String>,
}

/// One `OnCall` schedule (`GET /api/v1/schedules/…`). `on_call_now` holds the
/// `OnCall` user ids currently on call.
#[derive(Debug, Clone, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub on_call_now: Vec<String>,
}

/// A page of the schedules list endpoint (`{count, next, previous, results}`;
/// only `results` matters here).
#[derive(Debug, Clone, Deserialize)]
pub struct SchedulesPage {
    pub results: Vec<Schedule>,
}

/// True when the user is currently on call for the schedule.
pub fn is_on_duty(user_id: &str, schedule: &Schedule) -> bool {
    schedule.on_call_now.iter().any(|id| id == user_id)
}

/// Pick the schedule with an exactly matching name. The API's `name` query
/// filter should already narrow the page, but match defensively anyway.
pub fn find_schedule_by_name(page: SchedulesPage, name: &str) -> Option<Schedule> {
    page.results.into_iter().find(|s| s.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Realistic `/api/v1/users/current/` payload (extra fields must be
    /// tolerated).
    const USER_JSON: &str = r#"{
        "id": "U4DNY931HHJS5",
        "email": "alex@example.com",
        "slack": {"user_id": "UALEXSLACKID", "team_id": "TALEXSLACKID"},
        "username": "alex",
        "role": "admin",
        "timezone": "UTC",
        "teams": [],
        "is_phone_number_verified": false
    }"#;

    /// Realistic `/api/v1/schedules/` page.
    const SCHEDULES_JSON: &str = r#"{
        "count": 3,
        "next": null,
        "previous": null,
        "results": [
            {
                "id": "SBM7DV7BKFUYU",
                "name": "primary",
                "type": "ical",
                "on_call_now": ["U4DNY931HHJS5"],
                "slack": {"channel_id": "CH23212D", "user_group_id": null}
            },
            {
                "id": "S3Z7DV7BKFUYU",
                "name": "primary-eu",
                "type": "calendar",
                "on_call_now": []
            },
            {
                "id": "S9X7DV7BKFUYU",
                "name": "secondary"
            }
        ]
    }"#;

    #[test]
    fn current_user_parses_from_api_fixture() {
        let user: OnCallUser = serde_json::from_str(USER_JSON).expect("valid user");
        assert_eq!(user.id, "U4DNY931HHJS5");
        assert_eq!(user.email.as_deref(), Some("alex@example.com"));
        assert_eq!(user.username.as_deref(), Some("alex"));
    }

    #[test]
    fn schedules_page_parses_and_missing_on_call_now_defaults_empty() {
        let page: SchedulesPage = serde_json::from_str(SCHEDULES_JSON).expect("valid page");
        assert_eq!(page.results.len(), 3);
        assert_eq!(page.results[0].on_call_now, vec!["U4DNY931HHJS5"]);
        assert!(page.results[1].on_call_now.is_empty());
        // Third schedule omits the field entirely.
        assert!(page.results[2].on_call_now.is_empty());
    }

    #[test]
    fn is_on_duty_checks_membership() {
        let page: SchedulesPage = serde_json::from_str(SCHEDULES_JSON).expect("valid page");
        assert!(is_on_duty("U4DNY931HHJS5", &page.results[0]));
        assert!(!is_on_duty("USOMEONEELSE1", &page.results[0]));
        // Empty on_call_now: nobody is on duty.
        assert!(!is_on_duty("U4DNY931HHJS5", &page.results[1]));
    }

    #[test]
    fn find_schedule_by_name_requires_exact_match() {
        let page: SchedulesPage = serde_json::from_str(SCHEDULES_JSON).expect("valid page");
        // "primary" must not match the near-miss "primary-eu".
        let found = find_schedule_by_name(page.clone(), "primary").expect("found");
        assert_eq!(found.id, "SBM7DV7BKFUYU");
        assert!(find_schedule_by_name(page, "tertiary").is_none());
    }
}
