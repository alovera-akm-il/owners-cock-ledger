//! `reward_punishment_templates` (01-data-model.md §6) — the reusable
//! catalog a Keyholder builds up once, not per incident.

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

pub struct Template {
    pub id: String,
    // Not surfaced in TemplateResponse — a Keyholder listing their own
    // catalog already knows whose catalog it is.
    #[allow(dead_code)]
    pub keyholder_id: String,
    pub kind: String,
    pub title: String,
    pub description: Option<String>,
    pub severity: Option<i64>,
    pub active: bool,
    pub effect_kind: Option<String>,
    pub completion_type: Option<String>,
    pub proof_media_types: Option<String>,
    pub default_deadline_seconds: Option<i64>,
    pub time_extension_seconds: Option<i64>,
    pub time_reduction_seconds: Option<i64>,
    pub on_success_template_id: Option<String>,
    pub on_failure_template_id: Option<String>,
    pub points_delta: Option<i64>,
    pub points_cost: Option<i64>,
}

const COLUMNS: &str = "id, keyholder_id, kind, title, description, severity, active, \
     effect_kind, completion_type, proof_media_types, default_deadline_seconds, \
     time_extension_seconds, time_reduction_seconds, on_success_template_id, \
     on_failure_template_id, points_delta, points_cost";

fn row_to_template(row: &rusqlite::Row) -> rusqlite::Result<Template> {
    Ok(Template {
        id: row.get(0)?,
        keyholder_id: row.get(1)?,
        kind: row.get(2)?,
        title: row.get(3)?,
        description: row.get(4)?,
        severity: row.get(5)?,
        active: row.get(6)?,
        effect_kind: row.get(7)?,
        completion_type: row.get(8)?,
        proof_media_types: row.get(9)?,
        default_deadline_seconds: row.get(10)?,
        time_extension_seconds: row.get(11)?,
        time_reduction_seconds: row.get(12)?,
        on_success_template_id: row.get(13)?,
        on_failure_template_id: row.get(14)?,
        points_delta: row.get(15)?,
        points_cost: row.get(16)?,
    })
}

pub struct NewTemplate<'a> {
    pub kind: &'a str,
    pub title: &'a str,
    pub description: Option<&'a str>,
    pub severity: Option<i64>,
    pub effect_kind: Option<&'a str>,
    pub completion_type: Option<&'a str>,
    pub proof_media_types: Option<&'a str>,
    pub default_deadline_seconds: Option<i64>,
    pub time_extension_seconds: Option<i64>,
    pub time_reduction_seconds: Option<i64>,
    pub on_success_template_id: Option<&'a str>,
    pub on_failure_template_id: Option<&'a str>,
    pub points_delta: Option<i64>,
    pub points_cost: Option<i64>,
}

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("kind must be one of reward, punishment, task")]
    InvalidKind,
    #[error("effect_kind is required for reward/punishment templates")]
    MissingEffectKind,
    #[error("time_extension_seconds is required when effect_kind='time_extension'")]
    MissingTimeExtensionSeconds,
    #[error("time_reduction_seconds is required when effect_kind='time_reduction'")]
    MissingTimeReductionSeconds,
    #[error("completion_type is required for task templates")]
    MissingCompletionType,
    #[error("default_deadline_seconds is required for task templates")]
    MissingDefaultDeadlineSeconds,
    #[error("proof_media_types is required when completion_type='proof_required'")]
    MissingProofMediaTypes,
}

/// The `422`-worthy combination checks (03-api-design.md §7): required
/// fields differ by `kind`/`effect_kind`, and this is the one place that
/// logic lives rather than duplicated across create/update.
fn validate(new: &NewTemplate) -> Result<(), ValidationError> {
    match new.kind {
        "reward" | "punishment" => {
            let Some(effect_kind) = new.effect_kind else {
                return Err(ValidationError::MissingEffectKind);
            };
            if effect_kind == "time_extension" && new.time_extension_seconds.is_none() {
                return Err(ValidationError::MissingTimeExtensionSeconds);
            }
            if effect_kind == "time_reduction" && new.time_reduction_seconds.is_none() {
                return Err(ValidationError::MissingTimeReductionSeconds);
            }
        }
        "task" => {
            let Some(completion_type) = new.completion_type else {
                return Err(ValidationError::MissingCompletionType);
            };
            if new.default_deadline_seconds.is_none() {
                return Err(ValidationError::MissingDefaultDeadlineSeconds);
            }
            if completion_type == "proof_required" && new.proof_media_types.is_none() {
                return Err(ValidationError::MissingProofMediaTypes);
            }
        }
        _ => return Err(ValidationError::InvalidKind),
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum CreateError {
    #[error(transparent)]
    Validation(#[from] ValidationError),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub fn create(
    conn: &Connection,
    keyholder_id: &str,
    new: NewTemplate,
) -> Result<String, CreateError> {
    validate(&new)?;
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO reward_punishment_templates
            (id, keyholder_id, kind, title, description, severity, active, created_at,
             effect_kind, completion_type, proof_media_types, default_deadline_seconds,
             time_extension_seconds, time_reduction_seconds, on_success_template_id,
             on_failure_template_id, points_delta, points_cost)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            id,
            keyholder_id,
            new.kind,
            new.title,
            new.description,
            new.severity,
            crate::auth::session::now(),
            new.effect_kind,
            new.completion_type,
            new.proof_media_types,
            new.default_deadline_seconds,
            new.time_extension_seconds,
            new.time_reduction_seconds,
            new.on_success_template_id,
            new.on_failure_template_id,
            new.points_delta,
            new.points_cost,
        ],
    )?;
    Ok(id)
}

/// Loaded even when `active = false` — deactivating only hides a
/// template from being offered as a *new* choice, it doesn't retroactively
/// break an escalation chain already wired to it
/// (08-punishments-and-deadlines.md §6). Callers that need to enforce
/// "must be active" (a fresh assignment from the catalog) check
/// `.active` themselves.
pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<Template>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM reward_punishment_templates WHERE id = ?1"),
        params![id],
        row_to_template,
    )
    .optional()
}

pub fn list_for_keyholder(
    conn: &Connection,
    keyholder_id: &str,
    kind_filter: Option<&str>,
) -> rusqlite::Result<Vec<Template>> {
    let sql = match kind_filter {
        Some(_) => format!(
            "SELECT {COLUMNS} FROM reward_punishment_templates
             WHERE keyholder_id = ?1 AND kind = ?2 ORDER BY created_at DESC"
        ),
        None => format!(
            "SELECT {COLUMNS} FROM reward_punishment_templates
             WHERE keyholder_id = ?1 ORDER BY created_at DESC"
        ),
    };
    let mut stmt = conn.prepare(&sql)?;
    match kind_filter {
        Some(kind) => stmt
            .query_map(params![keyholder_id, kind], row_to_template)?
            .collect(),
        None => stmt
            .query_map(params![keyholder_id], row_to_template)?
            .collect(),
    }
}

/// A field left `None` here keeps its current value — the shape a
/// caller who only wants to reactivate (`{"active": true}`) needs.
/// `description`/`on_success_template_id`/`on_failure_template_id` are
/// double-`Option`ed (see `crate::api::templates::deserialize_some`) so
/// "omit this field" and "explicitly clear it" are distinguishable at
/// the API boundary — the other fields have no UI path that needs to
/// null them back out independently of setting a new value.
#[derive(Default)]
pub struct TemplateEdit<'a> {
    pub title: Option<&'a str>,
    pub description: Option<Option<&'a str>>,
    pub severity: Option<i64>,
    pub active: Option<bool>,
    pub effect_kind: Option<&'a str>,
    pub completion_type: Option<&'a str>,
    pub proof_media_types: Option<&'a str>,
    pub default_deadline_seconds: Option<i64>,
    pub time_extension_seconds: Option<i64>,
    pub time_reduction_seconds: Option<i64>,
    pub on_success_template_id: Option<Option<&'a str>>,
    pub on_failure_template_id: Option<Option<&'a str>>,
    pub points_delta: Option<i64>,
    pub points_cost: Option<i64>,
}

#[derive(Debug, Error)]
pub enum EditError {
    #[error("template not found or not yours")]
    NotFound,
    #[error(transparent)]
    Validation(#[from] ValidationError),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `PATCH /keyholder/templates/{id}` (03-api-design.md §7): edits any
/// combination of fields, re-validating the merged result against the
/// same required-field rules `create` enforces — `kind` itself is
/// immutable (changing it would invalidate whatever's already been
/// copied onto past assignments, 01-data-model.md §6). Never rewrites
/// past assignments either way; only future ones see the edit.
pub fn update(
    conn: &Connection,
    id: &str,
    keyholder_id: &str,
    edit: TemplateEdit,
) -> Result<(), EditError> {
    let current = get(conn, id)?
        .filter(|t| t.keyholder_id == keyholder_id)
        .ok_or(EditError::NotFound)?;

    let title = edit.title.unwrap_or(&current.title);
    let description = edit.description.unwrap_or(current.description.as_deref());
    let severity = edit.severity.or(current.severity);
    let effect_kind = edit.effect_kind.or(current.effect_kind.as_deref());
    let completion_type = edit.completion_type.or(current.completion_type.as_deref());
    let proof_media_types = edit
        .proof_media_types
        .or(current.proof_media_types.as_deref());
    let default_deadline_seconds = edit
        .default_deadline_seconds
        .or(current.default_deadline_seconds);
    let time_extension_seconds = edit
        .time_extension_seconds
        .or(current.time_extension_seconds);
    let time_reduction_seconds = edit
        .time_reduction_seconds
        .or(current.time_reduction_seconds);
    let on_success_template_id = edit
        .on_success_template_id
        .unwrap_or(current.on_success_template_id.as_deref());
    let on_failure_template_id = edit
        .on_failure_template_id
        .unwrap_or(current.on_failure_template_id.as_deref());
    let points_delta = edit.points_delta.or(current.points_delta);
    let points_cost = edit.points_cost.or(current.points_cost);
    let active = edit.active.unwrap_or(current.active);

    validate(&NewTemplate {
        kind: &current.kind,
        title,
        description,
        severity,
        effect_kind,
        completion_type,
        proof_media_types,
        default_deadline_seconds,
        time_extension_seconds,
        time_reduction_seconds,
        on_success_template_id,
        on_failure_template_id,
        points_delta,
        points_cost,
    })?;

    conn.execute(
        "UPDATE reward_punishment_templates SET
            title = ?1, description = ?2, severity = ?3, active = ?4,
            effect_kind = ?5, completion_type = ?6, proof_media_types = ?7,
            default_deadline_seconds = ?8, time_extension_seconds = ?9,
            time_reduction_seconds = ?10, on_success_template_id = ?11,
            on_failure_template_id = ?12, points_delta = ?13, points_cost = ?14
         WHERE id = ?15 AND keyholder_id = ?16",
        params![
            title,
            description,
            severity,
            active,
            effect_kind,
            completion_type,
            proof_media_types,
            default_deadline_seconds,
            time_extension_seconds,
            time_reduction_seconds,
            on_success_template_id,
            on_failure_template_id,
            points_delta,
            points_cost,
            id,
            keyholder_id,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_pool_with_keyholder() -> (tempfile::TempDir, crate::db::Pool, String) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'KH', 0)",
            params![id],
        )
        .unwrap();
        (dir, pool, id)
    }

    fn base_task() -> NewTemplate<'static> {
        NewTemplate {
            kind: "task",
            title: "cold shower",
            description: None,
            severity: None,
            effect_kind: None,
            completion_type: Some("proof_required"),
            proof_media_types: Some(r#"["video"]"#),
            default_deadline_seconds: Some(86_400),
            time_extension_seconds: None,
            time_reduction_seconds: None,
            on_success_template_id: None,
            on_failure_template_id: None,
            points_delta: None,
            points_cost: None,
        }
    }

    #[test]
    fn create_task_then_get_round_trips() {
        let (_dir, pool, kh) = temp_pool_with_keyholder();
        let conn = pool.get().unwrap();
        let id = create(&conn, &kh, base_task()).unwrap();

        let t = get(&conn, &id).unwrap().unwrap();
        assert_eq!(t.kind, "task");
        assert_eq!(t.completion_type.as_deref(), Some("proof_required"));
        assert!(t.active);
    }

    #[test]
    fn task_without_completion_type_is_rejected() {
        let (_dir, pool, kh) = temp_pool_with_keyholder();
        let conn = pool.get().unwrap();
        let mut new = base_task();
        new.completion_type = None;
        let result = create(&conn, &kh, new);
        assert!(matches!(
            result,
            Err(CreateError::Validation(
                ValidationError::MissingCompletionType
            ))
        ));
    }

    #[test]
    fn proof_required_task_without_media_types_is_rejected() {
        let (_dir, pool, kh) = temp_pool_with_keyholder();
        let conn = pool.get().unwrap();
        let mut new = base_task();
        new.proof_media_types = None;
        let result = create(&conn, &kh, new);
        assert!(matches!(
            result,
            Err(CreateError::Validation(
                ValidationError::MissingProofMediaTypes
            ))
        ));
    }

    #[test]
    fn punishment_time_extension_without_seconds_is_rejected() {
        let (_dir, pool, kh) = temp_pool_with_keyholder();
        let conn = pool.get().unwrap();
        let new = NewTemplate {
            kind: "punishment",
            title: "extra day locked",
            description: None,
            severity: None,
            effect_kind: Some("time_extension"),
            completion_type: None,
            proof_media_types: None,
            default_deadline_seconds: None,
            time_extension_seconds: None,
            time_reduction_seconds: None,
            on_success_template_id: None,
            on_failure_template_id: None,
            points_delta: None,
            points_cost: None,
        };
        let result = create(&conn, &kh, new);
        assert!(matches!(
            result,
            Err(CreateError::Validation(
                ValidationError::MissingTimeExtensionSeconds
            ))
        ));
    }

    fn deactivate(conn: &Connection, id: &str, keyholder_id: &str) -> Result<(), EditError> {
        update(
            conn,
            id,
            keyholder_id,
            TemplateEdit {
                active: Some(false),
                ..Default::default()
            },
        )
    }

    #[test]
    fn deactivate_leaves_the_row_gettable() {
        let (_dir, pool, kh) = temp_pool_with_keyholder();
        let conn = pool.get().unwrap();
        let id = create(&conn, &kh, base_task()).unwrap();
        deactivate(&conn, &id, &kh).unwrap();

        let t = get(&conn, &id).unwrap().unwrap();
        assert!(!t.active);
    }

    #[test]
    fn deactivate_is_scoped_to_owning_keyholder() {
        let (_dir, pool, kh) = temp_pool_with_keyholder();
        let conn = pool.get().unwrap();
        let id = create(&conn, &kh, base_task()).unwrap();
        let result = deactivate(&conn, &id, "someone-else");
        assert!(matches!(result, Err(EditError::NotFound)));
    }

    #[test]
    fn update_can_reactivate_without_touching_other_fields() {
        let (_dir, pool, kh) = temp_pool_with_keyholder();
        let conn = pool.get().unwrap();
        let id = create(&conn, &kh, base_task()).unwrap();
        deactivate(&conn, &id, &kh).unwrap();

        update(
            &conn,
            &id,
            &kh,
            TemplateEdit {
                active: Some(true),
                ..Default::default()
            },
        )
        .unwrap();

        let t = get(&conn, &id).unwrap().unwrap();
        assert!(t.active);
        assert_eq!(t.title, "cold shower");
        assert_eq!(t.default_deadline_seconds, Some(86_400));
    }

    #[test]
    fn update_can_edit_title_and_deadline() {
        let (_dir, pool, kh) = temp_pool_with_keyholder();
        let conn = pool.get().unwrap();
        let id = create(&conn, &kh, base_task()).unwrap();

        update(
            &conn,
            &id,
            &kh,
            TemplateEdit {
                title: Some("cold shower, extended"),
                default_deadline_seconds: Some(3600),
                ..Default::default()
            },
        )
        .unwrap();

        let t = get(&conn, &id).unwrap().unwrap();
        assert_eq!(t.title, "cold shower, extended");
        assert_eq!(t.default_deadline_seconds, Some(3600));
        // Untouched fields survive the edit.
        assert_eq!(t.completion_type.as_deref(), Some("proof_required"));
    }

    #[test]
    fn update_can_explicitly_clear_the_escalation_chain() {
        let (_dir, pool, kh) = temp_pool_with_keyholder();
        let conn = pool.get().unwrap();
        let target_id = create(&conn, &kh, base_task()).unwrap();
        let mut with_chain = base_task();
        with_chain.title = "has a chain";
        with_chain.on_failure_template_id = Some(&target_id);
        let id = create(&conn, &kh, with_chain).unwrap();
        assert!(
            get(&conn, &id)
                .unwrap()
                .unwrap()
                .on_failure_template_id
                .is_some()
        );

        update(
            &conn,
            &id,
            &kh,
            TemplateEdit {
                on_failure_template_id: Some(None),
                ..Default::default()
            },
        )
        .unwrap();

        assert!(
            get(&conn, &id)
                .unwrap()
                .unwrap()
                .on_failure_template_id
                .is_none()
        );
    }

    #[test]
    fn update_rejects_a_combination_that_would_fail_validation() {
        let (_dir, pool, kh) = temp_pool_with_keyholder();
        let conn = pool.get().unwrap();
        let id = create(
            &conn,
            &kh,
            NewTemplate {
                kind: "punishment",
                title: "corner time",
                description: None,
                severity: None,
                effect_kind: Some("grant"),
                completion_type: None,
                proof_media_types: None,
                default_deadline_seconds: None,
                time_extension_seconds: None,
                time_reduction_seconds: None,
                on_success_template_id: None,
                on_failure_template_id: None,
                points_delta: None,
                points_cost: None,
            },
        )
        .unwrap();

        // Switching to time_extension without ever setting the seconds
        // (neither previously nor in this edit) re-triggers the same
        // required-field check `create` enforces.
        let result = update(
            &conn,
            &id,
            &kh,
            TemplateEdit {
                effect_kind: Some("time_extension"),
                ..Default::default()
            },
        );
        assert!(matches!(
            result,
            Err(EditError::Validation(
                ValidationError::MissingTimeExtensionSeconds
            ))
        ));
        // Rejected edits don't partially apply.
        assert_eq!(
            get(&conn, &id).unwrap().unwrap().effect_kind.as_deref(),
            Some("grant")
        );
    }

    #[test]
    fn update_is_scoped_to_the_owning_keyholder() {
        let (_dir, pool, kh) = temp_pool_with_keyholder();
        let conn = pool.get().unwrap();
        let id = create(&conn, &kh, base_task()).unwrap();

        let result = update(
            &conn,
            &id,
            "someone-else",
            TemplateEdit {
                title: Some("hijacked"),
                ..Default::default()
            },
        );
        assert!(matches!(result, Err(EditError::NotFound)));
        assert_eq!(get(&conn, &id).unwrap().unwrap().title, "cold shower");
    }

    #[test]
    fn list_filters_by_kind() {
        let (_dir, pool, kh) = temp_pool_with_keyholder();
        let conn = pool.get().unwrap();
        create(&conn, &kh, base_task()).unwrap();
        create(
            &conn,
            &kh,
            NewTemplate {
                kind: "reward",
                title: "movie night",
                description: None,
                severity: None,
                effect_kind: Some("grant"),
                completion_type: None,
                proof_media_types: None,
                default_deadline_seconds: None,
                time_extension_seconds: None,
                time_reduction_seconds: None,
                on_success_template_id: None,
                on_failure_template_id: None,
                points_delta: None,
                points_cost: None,
            },
        )
        .unwrap();

        assert_eq!(list_for_keyholder(&conn, &kh, None).unwrap().len(), 2);
        assert_eq!(
            list_for_keyholder(&conn, &kh, Some("task")).unwrap().len(),
            1
        );
        assert_eq!(
            list_for_keyholder(&conn, &kh, Some("reward"))
                .unwrap()
                .len(),
            1
        );
    }
}
