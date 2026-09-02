//! Check-ins (01-data-model.md §14, 13-checkins.md): Keyholder-authored
//! templates with configurable custom fields, plus the always-present
//! `color` signal that's schema-level rather than just another field.
//! The real-time SSE fan-out for a live play-session check-in
//! (13-checkins.md §5) is Phase 7 — this module only covers the
//! ordinary create-once-rarely-edited REST shape every other check-in
//! (standalone, task-attached, confinement-attached) uses.

use rusqlite::{Connection, OptionalExtension, params};

use crate::domain::audit;

pub struct Template {
    pub id: String,
    pub keyholder_id: String,
    pub title: String,
    pub description: Option<String>,
    pub active: bool,
    pub auto_escalate_on_red: bool,
    pub created_at: i64,
}

const TEMPLATE_COLUMNS: &str =
    "id, keyholder_id, title, description, active, auto_escalate_on_red, created_at";

fn row_to_template(row: &rusqlite::Row) -> rusqlite::Result<Template> {
    Ok(Template {
        id: row.get(0)?,
        keyholder_id: row.get(1)?,
        title: row.get(2)?,
        description: row.get(3)?,
        active: row.get(4)?,
        auto_escalate_on_red: row.get(5)?,
        created_at: row.get(6)?,
    })
}

pub struct TemplateField {
    // Full mirror of the row; the API layer keys off `field_key`, not
    // this row's own id, and already returns fields in `position`
    // order so it doesn't need the raw number back out.
    #[allow(dead_code)]
    pub id: String,
    #[allow(dead_code)]
    pub position: i64,
    pub field_key: String,
    pub label: String,
    pub description: Option<String>,
    pub field_type: String,
    pub config: String,
    pub required: bool,
}

const FIELD_COLUMNS: &str =
    "id, position, field_key, label, description, field_type, config, required";

fn row_to_field(row: &rusqlite::Row) -> rusqlite::Result<TemplateField> {
    Ok(TemplateField {
        id: row.get(0)?,
        position: row.get(1)?,
        field_key: row.get(2)?,
        label: row.get(3)?,
        description: row.get(4)?,
        field_type: row.get(5)?,
        config: row.get(6)?,
        required: row.get(7)?,
    })
}

pub fn get_template(conn: &Connection, id: &str) -> rusqlite::Result<Option<Template>> {
    conn.query_row(
        &format!("SELECT {TEMPLATE_COLUMNS} FROM checkin_templates WHERE id = ?1"),
        params![id],
        row_to_template,
    )
    .optional()
}

pub fn list_fields(conn: &Connection, template_id: &str) -> rusqlite::Result<Vec<TemplateField>> {
    let sql = format!(
        "SELECT {FIELD_COLUMNS} FROM checkin_template_fields WHERE template_id = ?1 ORDER BY position ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map(params![template_id], row_to_field)?
        .collect()
}

pub fn list_templates_for_keyholder(
    conn: &Connection,
    keyholder_id: &str,
) -> rusqlite::Result<Vec<Template>> {
    let sql = format!(
        "SELECT {TEMPLATE_COLUMNS} FROM checkin_templates WHERE keyholder_id = ?1 ORDER BY created_at DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map(params![keyholder_id], row_to_template)?
        .collect()
}

pub struct NewField<'a> {
    pub field_key: &'a str,
    pub label: &'a str,
    pub description: Option<&'a str>,
    pub field_type: &'a str,
    pub config: &'a str,
    pub required: bool,
}

fn replace_fields(
    conn: &Connection,
    template_id: &str,
    fields: &[NewField],
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM checkin_template_fields WHERE template_id = ?1",
        params![template_id],
    )?;
    for (position, f) in fields.iter().enumerate() {
        conn.execute(
            "INSERT INTO checkin_template_fields
                (id, template_id, position, field_key, label, description, field_type, config, required)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                uuid::Uuid::new_v4().to_string(),
                template_id,
                position as i64,
                f.field_key,
                f.label,
                f.description,
                f.field_type,
                f.config,
                f.required,
            ],
        )?;
    }
    Ok(())
}

/// `POST /keyholder/checkin-templates` (03-api-design.md §10b).
pub fn create_template(
    conn: &Connection,
    keyholder_id: &str,
    title: &str,
    description: Option<&str>,
    auto_escalate_on_red: bool,
    fields: &[NewField],
) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO checkin_templates (id, keyholder_id, title, description, active, auto_escalate_on_red, created_at)
         VALUES (?1,?2,?3,?4,1,?5,?6)",
        params![id, keyholder_id, title, description, auto_escalate_on_red, crate::auth::session::now()],
    )?;
    replace_fields(conn, &id, fields)?;
    Ok(id)
}

#[derive(Default)]
pub struct TemplateEdit<'a> {
    pub title: Option<&'a str>,
    pub description: Option<Option<&'a str>>,
    pub auto_escalate_on_red: Option<bool>,
    pub active: Option<bool>,
    /// `Some` replaces the whole field list (never partially patched —
    /// the Keyholder's edit form always sends the complete set);
    /// `None` leaves existing fields untouched.
    pub fields: Option<Vec<NewField<'a>>>,
}

/// `PATCH /keyholder/checkin-templates/{id}` — editing fields never
/// rewrites `field_values` already recorded on past `checkins` rows
/// (03-api-design.md §10b), since those are a freestanding JSON blob
/// keyed by `field_key`, not a foreign key into this table. Returns
/// `false` if no such template belongs to this Keyholder.
pub fn update_template(
    conn: &Connection,
    id: &str,
    keyholder_id: &str,
    edit: TemplateEdit,
) -> rusqlite::Result<bool> {
    let Some(current) = get_template(conn, id)? else {
        return Ok(false);
    };
    if current.keyholder_id != keyholder_id {
        return Ok(false);
    }
    let title = edit.title.unwrap_or(&current.title);
    let description = edit.description.unwrap_or(current.description.as_deref());
    let auto_escalate_on_red = edit
        .auto_escalate_on_red
        .unwrap_or(current.auto_escalate_on_red);
    let active = edit.active.unwrap_or(current.active);

    conn.execute(
        "UPDATE checkin_templates SET title = ?1, description = ?2, auto_escalate_on_red = ?3, active = ?4
         WHERE id = ?5",
        params![title, description, auto_escalate_on_red, active, id],
    )?;
    if let Some(fields) = edit.fields {
        replace_fields(conn, id, &fields)?;
    }
    Ok(true)
}

pub struct Checkin {
    pub id: String,
    pub link_id: String,
    pub template_id: String,
    pub color: String,
    pub field_values: String,
    pub related_confinement_session_id: Option<String>,
    pub related_assignment_id: Option<String>,
    pub related_play_session_id: Option<String>,
    pub created_by_user_id: String,
    #[allow(dead_code)]
    pub updated_by_user_id: Option<String>,
    pub created_at: i64,
    #[allow(dead_code)]
    pub updated_at: i64,
}

const CHECKIN_COLUMNS: &str = "id, link_id, template_id, color, field_values, \
     related_confinement_session_id, related_assignment_id, related_play_session_id, \
     created_by_user_id, updated_by_user_id, created_at, updated_at";

fn row_to_checkin(row: &rusqlite::Row) -> rusqlite::Result<Checkin> {
    Ok(Checkin {
        id: row.get(0)?,
        link_id: row.get(1)?,
        template_id: row.get(2)?,
        color: row.get(3)?,
        field_values: row.get(4)?,
        related_confinement_session_id: row.get(5)?,
        related_assignment_id: row.get(6)?,
        related_play_session_id: row.get(7)?,
        created_by_user_id: row.get(8)?,
        updated_by_user_id: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

pub fn get_checkin(conn: &Connection, id: &str) -> rusqlite::Result<Option<Checkin>> {
    conn.query_row(
        &format!("SELECT {CHECKIN_COLUMNS} FROM checkins WHERE id = ?1"),
        params![id],
        row_to_checkin,
    )
    .optional()
}

pub struct CheckinFilter<'a> {
    pub color: Option<&'a str>,
    pub related_assignment_id: Option<&'a str>,
    pub related_confinement_session_id: Option<&'a str>,
    pub related_play_session_id: Option<&'a str>,
}

pub fn list_for_link(
    conn: &Connection,
    link_id: &str,
    filter: CheckinFilter,
) -> rusqlite::Result<Vec<Checkin>> {
    let mut sql = format!("SELECT {CHECKIN_COLUMNS} FROM checkins WHERE link_id = ?1");
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(link_id.to_string())];
    if let Some(c) = filter.color {
        sql.push_str(" AND color = ?");
        params.push(Box::new(c.to_string()));
    }
    if let Some(a) = filter.related_assignment_id {
        sql.push_str(" AND related_assignment_id = ?");
        params.push(Box::new(a.to_string()));
    }
    if let Some(s) = filter.related_confinement_session_id {
        sql.push_str(" AND related_confinement_session_id = ?");
        params.push(Box::new(s.to_string()));
    }
    if let Some(p) = filter.related_play_session_id {
        sql.push_str(" AND related_play_session_id = ?");
        params.push(Box::new(p.to_string()));
    }
    sql.push_str(" ORDER BY created_at DESC");
    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    stmt.query_map(param_refs.as_slice(), row_to_checkin)?
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum CreateCheckinError {
    #[error("template not found, or not this link's own")]
    TemplateNotFound,
    #[error("missing a required field")]
    MissingRequiredField,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub struct NewCheckin<'a> {
    pub link_id: &'a str,
    pub template_id: &'a str,
    pub color: &'a str,
    pub field_values: &'a str,
    pub related_confinement_session_id: Option<&'a str>,
    pub related_assignment_id: Option<&'a str>,
    pub related_play_session_id: Option<&'a str>,
    pub created_by_user_id: &'a str,
}

fn missing_required_field(field_values: &str, fields: &[TemplateField]) -> bool {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(field_values) else {
        return true;
    };
    let Some(obj) = parsed.as_object() else {
        return true;
    };
    fields
        .iter()
        .any(|f| f.required && !obj.get(&f.field_key).is_some_and(|v| !v.is_null()))
}

/// `POST .../checkins` — either role, for their own link
/// (03-api-design.md §10b). When the color is (freshly) `red` and the
/// template opted into `auto_escalate_on_red`, raises a `safety_alerts`
/// row in the same transaction (13-checkins.md §6) and returns its id
/// so the caller can fire the matching notification — this module
/// stays push-agnostic, same as every other domain module.
pub fn create_checkin(
    conn: &mut Connection,
    new: NewCheckin,
    submissive_id: &str,
) -> Result<(String, Option<String>), CreateCheckinError> {
    let tx = conn.transaction()?;

    let Some(template) = get_template(&tx, new.template_id)? else {
        return Err(CreateCheckinError::TemplateNotFound);
    };
    let fields = list_fields(&tx, new.template_id)?;
    if missing_required_field(new.field_values, &fields) {
        return Err(CreateCheckinError::MissingRequiredField);
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::auth::session::now();
    tx.execute(
        "INSERT INTO checkins
            (id, link_id, template_id, color, field_values, related_confinement_session_id,
             related_assignment_id, related_play_session_id, created_by_user_id, created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)",
        params![
            id,
            new.link_id,
            new.template_id,
            new.color,
            new.field_values,
            new.related_confinement_session_id,
            new.related_assignment_id,
            new.related_play_session_id,
            new.created_by_user_id,
            now,
        ],
    )?;

    let mut alert_id = None;
    if new.color == "red" && template.auto_escalate_on_red {
        let new_alert_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO safety_alerts
                (id, submissive_id, link_id, raised_at, raised_via, related_checkin_id, message)
             VALUES (?1, ?2, ?3, ?4, 'system', ?5, ?6)",
            params![
                new_alert_id,
                submissive_id,
                new.link_id,
                now,
                id,
                format!("Auto-raised: RED on '{}'", template.title),
            ],
        )?;
        audit::record(
            &tx,
            audit::Entry {
                actor: audit::Actor::System,
                link_id: Some(new.link_id),
                action: "safety_alert.raised",
                entity_type: "safety_alerts",
                entity_id: &new_alert_id,
                detail: None,
            },
        )?;
        alert_id = Some(new_alert_id);
    }

    tx.commit()?;
    Ok((id, alert_id))
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateCheckinError {
    #[error("check-in not found")]
    NotFound,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

#[derive(Default)]
pub struct CheckinEdit<'a> {
    pub color: Option<&'a str>,
    pub field_values: Option<&'a str>,
}

/// `PATCH /checkins/{id}` — either party, on their own link
/// (03-api-design.md §10b). Raises a safety alert the same way
/// `create_checkin` does, but only when the color transitions *into*
/// red (was non-red, now red) — a follow-up edit to an already-red
/// check-in never raises a second one (13-checkins.md §6).
pub fn update_checkin(
    conn: &mut Connection,
    id: &str,
    edit: CheckinEdit,
    submissive_id: &str,
    updated_by_user_id: &str,
) -> Result<Option<String>, UpdateCheckinError> {
    let tx = conn.transaction()?;

    let Some(current) = get_checkin(&tx, id)? else {
        return Err(UpdateCheckinError::NotFound);
    };
    let new_color = edit.color.unwrap_or(&current.color);
    let new_field_values = edit.field_values.unwrap_or(&current.field_values);
    let now = crate::auth::session::now();

    tx.execute(
        "UPDATE checkins SET color = ?1, field_values = ?2, updated_by_user_id = ?3, updated_at = ?4
         WHERE id = ?5",
        params![new_color, new_field_values, updated_by_user_id, now, id],
    )?;

    let mut alert_id = None;
    if new_color == "red" && current.color != "red" {
        let template = get_template(&tx, &current.template_id)?;
        if template.is_some_and(|t| t.auto_escalate_on_red) {
            let new_alert_id = uuid::Uuid::new_v4().to_string();
            let title: String = tx.query_row(
                "SELECT title FROM checkin_templates WHERE id = ?1",
                params![current.template_id],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT INTO safety_alerts
                    (id, submissive_id, link_id, raised_at, raised_via, related_checkin_id, message)
                 VALUES (?1, ?2, ?3, ?4, 'system', ?5, ?6)",
                params![
                    new_alert_id,
                    submissive_id,
                    current.link_id,
                    now,
                    id,
                    format!("Auto-raised: RED on '{title}'"),
                ],
            )?;
            audit::record(
                &tx,
                audit::Entry {
                    actor: audit::Actor::System,
                    link_id: Some(&current.link_id),
                    action: "safety_alert.raised",
                    entity_type: "safety_alerts",
                    entity_id: &new_alert_id,
                    detail: None,
                },
            )?;
            alert_id = Some(new_alert_id);
        }
    }

    tx.commit()?;
    Ok(alert_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_pool_with_link() -> (tempfile::TempDir, crate::db::Pool, String, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let keyholder_id = uuid::Uuid::new_v4().to_string();
        let submissive_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'KH', 0)",
            params![keyholder_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'submissive', 'Sub', 0)",
            params![submissive_id],
        )
        .unwrap();
        let link_id = crate::domain::links::create(&conn, &keyholder_id, &submissive_id).unwrap();
        (dir, pool, keyholder_id, submissive_id, link_id)
    }

    fn seed_assignment(conn: &Connection, link_id: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO assignments (id, link_id, kind, title, assigned_at, assigned_via, status)
             VALUES (?1, ?2, 'task', 'test task', 0, 'session', 'assigned')",
            params![id, link_id],
        )
        .unwrap();
        id
    }

    fn seed_template(conn: &Connection, kh: &str, auto_escalate: bool) -> String {
        create_template(
            conn,
            kh,
            "Morning check-in",
            None,
            auto_escalate,
            &[NewField {
                field_key: "skin_status",
                label: "Skin status",
                description: None,
                field_type: "text",
                config: "{}",
                required: true,
            }],
        )
        .unwrap()
    }

    #[test]
    fn create_template_and_list_fields_round_trips() {
        let (_dir, pool, kh, _sub, _link) = temp_pool_with_link();
        let conn = pool.get().unwrap();
        let template_id = seed_template(&conn, &kh, false);

        let fields = list_fields(&conn, &template_id).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_key, "skin_status");
        assert!(fields[0].required);
    }

    #[test]
    fn update_template_replaces_fields_without_touching_past_checkins() {
        let (_dir, pool, kh, sub, link) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();
        let template_id = seed_template(&conn, &kh, false);

        let (checkin_id, alert) = create_checkin(
            &mut conn,
            NewCheckin {
                link_id: &link,
                template_id: &template_id,
                color: "green",
                field_values: r#"{"skin_status":"normal"}"#,
                related_confinement_session_id: None,
                related_assignment_id: None,
                related_play_session_id: None,
                created_by_user_id: &sub,
            },
            &sub,
        )
        .unwrap();
        assert!(alert.is_none());

        update_template(
            &conn,
            &template_id,
            &kh,
            TemplateEdit {
                fields: Some(vec![NewField {
                    field_key: "mood",
                    label: "Mood",
                    description: None,
                    field_type: "text",
                    config: "{}",
                    required: false,
                }]),
                ..Default::default()
            },
        )
        .unwrap();

        // The template's fields changed...
        let fields = list_fields(&conn, &template_id).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_key, "mood");
        // ...but the already-recorded check-in's own field_values are untouched.
        let checkin = get_checkin(&conn, &checkin_id).unwrap().unwrap();
        assert_eq!(checkin.field_values, r#"{"skin_status":"normal"}"#);
    }

    #[test]
    fn create_checkin_rejects_a_missing_required_field() {
        let (_dir, pool, kh, sub, link) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();
        let template_id = seed_template(&conn, &kh, false);

        let result = create_checkin(
            &mut conn,
            NewCheckin {
                link_id: &link,
                template_id: &template_id,
                color: "green",
                field_values: "{}",
                related_confinement_session_id: None,
                related_assignment_id: None,
                related_play_session_id: None,
                created_by_user_id: &sub,
            },
            &sub,
        );
        assert!(matches!(
            result,
            Err(CreateCheckinError::MissingRequiredField)
        ));
    }

    #[test]
    fn create_checkin_red_with_auto_escalate_raises_a_safety_alert() {
        let (_dir, pool, kh, sub, link) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();
        let template_id = seed_template(&conn, &kh, true);

        let (checkin_id, alert_id) = create_checkin(
            &mut conn,
            NewCheckin {
                link_id: &link,
                template_id: &template_id,
                color: "red",
                field_values: r#"{"skin_status":"open skin"}"#,
                related_confinement_session_id: None,
                related_assignment_id: None,
                related_play_session_id: None,
                created_by_user_id: &sub,
            },
            &sub,
        )
        .unwrap();
        let alert_id = alert_id.expect("red + auto_escalate raises an alert");

        let related: Option<String> = conn
            .query_row(
                "SELECT related_checkin_id FROM safety_alerts WHERE id = ?1",
                params![alert_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(related.as_deref(), Some(checkin_id.as_str()));

        let raised_via: String = conn
            .query_row(
                "SELECT raised_via FROM safety_alerts WHERE id = ?1",
                params![alert_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raised_via, "system");
    }

    #[test]
    fn create_checkin_green_never_raises_even_with_auto_escalate_on() {
        let (_dir, pool, kh, sub, link) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();
        let template_id = seed_template(&conn, &kh, true);

        let (_id, alert_id) = create_checkin(
            &mut conn,
            NewCheckin {
                link_id: &link,
                template_id: &template_id,
                color: "green",
                field_values: r#"{"skin_status":"normal"}"#,
                related_confinement_session_id: None,
                related_assignment_id: None,
                related_play_session_id: None,
                created_by_user_id: &sub,
            },
            &sub,
        )
        .unwrap();
        assert!(alert_id.is_none());
    }

    #[test]
    fn update_checkin_only_raises_on_the_transition_into_red_not_every_save() {
        let (_dir, pool, kh, sub, link) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();
        let template_id = seed_template(&conn, &kh, true);

        let (checkin_id, first_alert) = create_checkin(
            &mut conn,
            NewCheckin {
                link_id: &link,
                template_id: &template_id,
                color: "red",
                field_values: r#"{"skin_status":"open skin"}"#,
                related_confinement_session_id: None,
                related_assignment_id: None,
                related_play_session_id: None,
                created_by_user_id: &sub,
            },
            &sub,
        )
        .unwrap();
        assert!(first_alert.is_some());

        // A follow-up edit that's still red (e.g. adding a note) must not
        // raise a second alert.
        let second_alert = update_checkin(
            &mut conn,
            &checkin_id,
            CheckinEdit {
                field_values: Some(r#"{"skin_status":"open skin","notes":"still bad"}"#),
                ..Default::default()
            },
            &sub,
            &kh,
        )
        .unwrap();
        assert!(second_alert.is_none());

        let alert_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM safety_alerts WHERE related_checkin_id = ?1",
                params![checkin_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(alert_count, 1);

        // Leaving red and coming back DOES raise a fresh one.
        update_checkin(
            &mut conn,
            &checkin_id,
            CheckinEdit {
                color: Some("green"),
                ..Default::default()
            },
            &sub,
            &kh,
        )
        .unwrap();
        let third_alert = update_checkin(
            &mut conn,
            &checkin_id,
            CheckinEdit {
                color: Some("red"),
                ..Default::default()
            },
            &sub,
            &kh,
        )
        .unwrap();
        assert!(third_alert.is_some());
    }

    #[test]
    fn list_for_link_filters_by_color_and_related_assignment() {
        let (_dir, pool, kh, sub, link) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();
        let template_id = seed_template(&conn, &kh, false);
        let assignment_id = seed_assignment(&conn, &link);

        create_checkin(
            &mut conn,
            NewCheckin {
                link_id: &link,
                template_id: &template_id,
                color: "green",
                field_values: r#"{"skin_status":"normal"}"#,
                related_confinement_session_id: None,
                related_assignment_id: Some(&assignment_id),
                related_play_session_id: None,
                created_by_user_id: &sub,
            },
            &sub,
        )
        .unwrap();
        create_checkin(
            &mut conn,
            NewCheckin {
                link_id: &link,
                template_id: &template_id,
                color: "yellow",
                field_values: r#"{"skin_status":"mild redness"}"#,
                related_confinement_session_id: None,
                related_assignment_id: None,
                related_play_session_id: None,
                created_by_user_id: &sub,
            },
            &sub,
        )
        .unwrap();

        let conn = pool.get().unwrap();
        assert_eq!(
            list_for_link(
                &conn,
                &link,
                CheckinFilter {
                    color: None,
                    related_assignment_id: None,
                    related_confinement_session_id: None,
                    related_play_session_id: None,
                },
            )
            .unwrap()
            .len(),
            2
        );
        assert_eq!(
            list_for_link(
                &conn,
                &link,
                CheckinFilter {
                    color: Some("yellow"),
                    related_assignment_id: None,
                    related_confinement_session_id: None,
                    related_play_session_id: None,
                },
            )
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            list_for_link(
                &conn,
                &link,
                CheckinFilter {
                    color: None,
                    related_assignment_id: Some(&assignment_id),
                    related_confinement_session_id: None,
                    related_play_session_id: None,
                },
            )
            .unwrap()
            .len(),
            1
        );
    }
}
