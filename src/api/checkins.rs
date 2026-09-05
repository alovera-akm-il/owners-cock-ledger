//! Check-in templates and instances (03-api-design.md §10b).

use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::session::{CurrentUser, Role};
use crate::db::{self, BlobDir, Pool};
use crate::domain::{checkins, links, play_sessions};
use crate::live::PlaySessionStreams;
use crate::notify;

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found");
const BAD_REQUEST: ApiError =
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "malformed request");
const BAD_PHOTO: ApiError = ApiError::new(
    StatusCode::BAD_REQUEST,
    "bad_request",
    "expected a single 'photo' field with an image/jpeg, image/png, video/mp4, or video/webm file",
);
const BAD_AUDIO: ApiError = ApiError::new(
    StatusCode::BAD_REQUEST,
    "bad_request",
    "expected a single 'audio' field with an audio/webm, audio/mp4, audio/mpeg (mp3), or audio/wav file",
);
const MAX_CHECKIN_MEDIA_BYTES: usize = 400 * 1024 * 1024;

fn valid_photo_content_type(t: &str) -> bool {
    matches!(t, "image/jpeg" | "image/png" | "video/mp4" | "video/webm")
}

fn valid_audio_content_type(t: &str) -> bool {
    matches!(
        t,
        "audio/webm" | "audio/mp4" | "audio/mpeg" | "audio/mp3" | "audio/wav" | "audio/x-wav" | "audio/wave"
    )
}

#[derive(Serialize)]
struct FieldResponse {
    field_key: String,
    label: String,
    description: Option<String>,
    field_type: String,
    config: serde_json::Value,
    required: bool,
}

impl From<checkins::TemplateField> for FieldResponse {
    fn from(f: checkins::TemplateField) -> Self {
        Self {
            field_key: f.field_key,
            label: f.label,
            description: f.description,
            field_type: f.field_type,
            config: serde_json::from_str(&f.config).unwrap_or(serde_json::Value::Null),
            required: f.required,
        }
    }
}

#[derive(Serialize)]
struct TemplateResponse {
    id: String,
    title: String,
    description: Option<String>,
    active: bool,
    auto_escalate_on_red: bool,
    created_at: String,
    fields: Vec<FieldResponse>,
}

fn template_response(
    t: checkins::Template,
    fields: Vec<checkins::TemplateField>,
) -> TemplateResponse {
    TemplateResponse {
        id: t.id,
        title: t.title,
        description: t.description,
        active: t.active,
        auto_escalate_on_red: t.auto_escalate_on_red,
        created_at: iso8601(t.created_at),
        fields: fields.into_iter().map(Into::into).collect(),
    }
}

#[derive(Deserialize)]
struct FieldRequest {
    field_key: String,
    label: String,
    description: Option<String>,
    field_type: String,
    config: serde_json::Value,
    #[serde(default)]
    required: bool,
}

async fn list_templates(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<TemplateResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let list = checkins::list_templates_for_keyholder(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?;
        let mut out = Vec::with_capacity(list.len());
        for t in list {
            let fields = checkins::list_fields(&conn, &t.id).map_err(|_| INTERNAL_ERROR)?;
            out.push(template_response(t, fields));
        }
        Ok(Json(out))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn list_templates_for_submissive(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<TemplateResponse>>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = links::active_link_for_submissive(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let settings = links::settings_for_link(&conn, &link_id).map_err(|_| INTERNAL_ERROR)?;
        if !settings.catalog_visible_to_submissive {
            return Ok(Json(Vec::new()));
        }
        let (keyholder_id, _) = links::parties(&conn, &link_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)?;
        let list = checkins::list_templates_for_keyholder(&conn, &keyholder_id)
            .map_err(|_| INTERNAL_ERROR)?
            .into_iter()
            .filter(|t| t.active);
        let mut out = Vec::new();
        for t in list {
            let fields = checkins::list_fields(&conn, &t.id).map_err(|_| INTERNAL_ERROR)?;
            out.push(template_response(t, fields));
        }
        Ok(Json(out))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
struct CreateTemplateRequest {
    title: String,
    description: Option<String>,
    #[serde(default)]
    auto_escalate_on_red: bool,
    fields: Vec<FieldRequest>,
}

fn valid_field_type(t: &str) -> bool {
    matches!(
        t,
        "scale" | "select" | "number" | "text" | "boolean" | "photo" | "audio"
    )
}

async fn create_template(
    State(pool): State<Pool>,
    user: CurrentUser,
    Json(req): Json<CreateTemplateRequest>,
) -> Result<Json<TemplateResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    if req.fields.iter().any(|f| !valid_field_type(&f.field_type)) {
        return Err(BAD_REQUEST);
    }
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let configs: Vec<String> = req.fields.iter().map(|f| f.config.to_string()).collect();
        let new_fields: Vec<checkins::NewField> = req
            .fields
            .iter()
            .zip(configs.iter())
            .map(|(f, config)| checkins::NewField {
                field_key: &f.field_key,
                label: &f.label,
                description: f.description.as_deref(),
                field_type: &f.field_type,
                config,
                required: f.required,
            })
            .collect();
        let id = checkins::create_template(
            &conn,
            &user.user_id,
            &req.title,
            req.description.as_deref(),
            req.auto_escalate_on_red,
            &new_fields,
        )
        .map_err(|_| INTERNAL_ERROR)?;
        let t = checkins::get_template(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)?;
        let fields = checkins::list_fields(&conn, &id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(template_response(t, fields)))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
struct PatchTemplateRequest {
    title: Option<String>,
    description: Option<Option<String>>,
    auto_escalate_on_red: Option<bool>,
    active: Option<bool>,
    fields: Option<Vec<FieldRequest>>,
}

async fn patch_template(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
    Json(req): Json<PatchTemplateRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    if let Some(fields) = &req.fields
        && fields.iter().any(|f| !valid_field_type(&f.field_type))
    {
        return Err(BAD_REQUEST);
    }
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let configs: Option<Vec<String>> = req
            .fields
            .as_ref()
            .map(|fs| fs.iter().map(|f| f.config.to_string()).collect());
        let new_fields: Option<Vec<checkins::NewField>> = match (&req.fields, &configs) {
            (Some(fields), Some(configs)) => Some(
                fields
                    .iter()
                    .zip(configs.iter())
                    .map(|(f, config)| checkins::NewField {
                        field_key: &f.field_key,
                        label: &f.label,
                        description: f.description.as_deref(),
                        field_type: &f.field_type,
                        config,
                        required: f.required,
                    })
                    .collect(),
            ),
            _ => None,
        };
        let updated = checkins::update_template(
            &conn,
            &id,
            &user.user_id,
            checkins::TemplateEdit {
                title: req.title.as_deref(),
                description: req.description.as_ref().map(|d| d.as_deref()),
                auto_escalate_on_red: req.auto_escalate_on_red,
                active: req.active,
                fields: new_fields,
            },
        )
        .map_err(|_| INTERNAL_ERROR)?;
        if !updated {
            return Err(NOT_FOUND);
        }
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Serialize)]
struct CheckinResponse {
    id: String,
    link_id: String,
    template_id: String,
    color: String,
    field_values: serde_json::Value,
    related_confinement_session_id: Option<String>,
    related_assignment_id: Option<String>,
    related_play_session_id: Option<String>,
    created_by_user_id: String,
    created_at: String,
    photo_url: Option<String>,
    photo_mime_type: Option<String>,
    audio_url: Option<String>,
}

impl From<checkins::Checkin> for CheckinResponse {
    fn from(c: checkins::Checkin) -> Self {
        Self {
            photo_url: c
                .photo_attachment_path
                .is_some()
                .then(|| format!("/api/v1/checkins/{}/photo", c.id)),
            photo_mime_type: c.photo_mime_type.clone(),
            audio_url: c
                .audio_attachment_path
                .is_some()
                .then(|| format!("/api/v1/checkins/{}/audio", c.id)),
            id: c.id,
            link_id: c.link_id,
            template_id: c.template_id,
            color: c.color,
            field_values: serde_json::from_str(&c.field_values).unwrap_or(serde_json::Value::Null),
            related_confinement_session_id: c.related_confinement_session_id,
            related_assignment_id: c.related_assignment_id,
            related_play_session_id: c.related_play_session_id,
            created_by_user_id: c.created_by_user_id,
            created_at: iso8601(c.created_at),
        }
    }
}

fn valid_color(c: &str) -> bool {
    matches!(c, "green" | "yellow" | "red")
}

/// The multipart shape both create routes share. `field_values` travels
/// as a JSON-text field (same trick `proofs.rs` uses for its `metadata`
/// field) rather than the request's native JSON body, since a request
/// that might also carry a `photo` file has to be multipart as a whole —
/// Axum doesn't offer a "JSON body, but also maybe a file" extractor.
struct ParsedCheckinCreate {
    template_id: String,
    color: String,
    field_values: String,
    related_confinement_session_id: Option<String>,
    related_assignment_id: Option<String>,
    related_play_session_id: Option<String>,
    photo: Option<(String, Vec<u8>)>,
    audio: Option<(String, Vec<u8>)>,
}

async fn parse_checkin_multipart(mut multipart: Multipart) -> Result<ParsedCheckinCreate, ApiError> {
    let mut template_id = None;
    let mut color = None;
    let mut field_values = None;
    let mut related_confinement_session_id = None;
    let mut related_assignment_id = None;
    let mut related_play_session_id = None;
    let mut photo = None;
    let mut audio = None;

    while let Some(field) = multipart.next_field().await.map_err(|_| BAD_REQUEST)? {
        match field.name().unwrap_or("") {
            "template_id" => template_id = Some(field.text().await.map_err(|_| BAD_REQUEST)?),
            "color" => color = Some(field.text().await.map_err(|_| BAD_REQUEST)?),
            "field_values" => field_values = Some(field.text().await.map_err(|_| BAD_REQUEST)?),
            "related_confinement_session_id" => {
                let v = field.text().await.map_err(|_| BAD_REQUEST)?;
                related_confinement_session_id = (!v.is_empty()).then_some(v);
            }
            "related_assignment_id" => {
                let v = field.text().await.map_err(|_| BAD_REQUEST)?;
                related_assignment_id = (!v.is_empty()).then_some(v);
            }
            "related_play_session_id" => {
                let v = field.text().await.map_err(|_| BAD_REQUEST)?;
                related_play_session_id = (!v.is_empty()).then_some(v);
            }
            "photo" => {
                let content_type = field
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let bytes = field.bytes().await.map_err(|_| BAD_PHOTO)?.to_vec();
                photo = Some((content_type, bytes));
            }
            "audio" => {
                let content_type = field
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let bytes = field.bytes().await.map_err(|_| BAD_AUDIO)?.to_vec();
                audio = Some((content_type, bytes));
            }
            _ => {
                let _ = field.bytes().await;
            }
        }
    }

    if let Some((content_type, _)) = &photo
        && !valid_photo_content_type(content_type)
    {
        return Err(BAD_PHOTO);
    }
    if let Some((content_type, _)) = &audio
        && !valid_audio_content_type(content_type)
    {
        return Err(BAD_AUDIO);
    }

    Ok(ParsedCheckinCreate {
        template_id: template_id.ok_or(BAD_REQUEST)?,
        color: color.ok_or(BAD_REQUEST)?,
        field_values: field_values.unwrap_or_else(|| "{}".to_string()),
        related_confinement_session_id,
        related_assignment_id,
        related_play_session_id,
        photo,
        audio,
    })
}

async fn create_for_keyholder(
    State(pool): State<Pool>,
    State(BlobDir(blob_dir)): State<BlobDir>,
    State(streams): State<PlaySessionStreams>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    multipart: Multipart,
) -> Result<Json<CheckinResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    let parsed = parse_checkin_multipart(multipart).await?;
    if !valid_color(&parsed.color) {
        return Err(BAD_REQUEST);
    }
    let pool2 = pool.clone();
    let (checkin, alert_id, keyholder_id) =
        tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
            let mut conn = pool2.get().map_err(|_| INTERNAL_ERROR)?;
            let link_id =
                links::active_or_paused_link_for_keyholder(&conn, &user.user_id, &submissive_id)
                    .map_err(|_| INTERNAL_ERROR)?
                    .ok_or(NOT_FOUND)?;
            let (id, alert_id) = checkins::create_checkin(
                &mut conn,
                checkins::NewCheckin {
                    link_id: &link_id,
                    template_id: &parsed.template_id,
                    color: &parsed.color,
                    field_values: &parsed.field_values,
                    related_confinement_session_id: parsed.related_confinement_session_id.as_deref(),
                    related_assignment_id: parsed.related_assignment_id.as_deref(),
                    related_play_session_id: parsed.related_play_session_id.as_deref(),
                    created_by_user_id: &user.user_id,
                    has_photo: parsed.photo.is_some(),
                    has_audio: parsed.audio.is_some(),
                },
                &submissive_id,
            )
            .map_err(|e| match e {
                checkins::CreateCheckinError::TemplateNotFound => NOT_FOUND,
                checkins::CreateCheckinError::MissingRequiredField => BAD_REQUEST,
                checkins::CreateCheckinError::Db(_) => INTERNAL_ERROR,
            })?;
            if let Some((content_type, bytes)) = &parsed.photo {
                let stored = crate::storage::store(&blob_dir, content_type, bytes)
                    .map_err(|_| BAD_PHOTO)?;
                checkins::set_photo(&conn, &id, &stored.storage_path, content_type)
                    .map_err(|_| INTERNAL_ERROR)?;
            }
            if let Some((content_type, bytes)) = &parsed.audio {
                let stored = crate::storage::store(&blob_dir, content_type, bytes)
                    .map_err(|_| BAD_AUDIO)?;
                checkins::set_audio(&conn, &id, &stored.storage_path, content_type)
                    .map_err(|_| INTERNAL_ERROR)?;
            }
            let checkin = checkins::get_checkin(&conn, &id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(INTERNAL_ERROR)?;
            if let Some(play_session_id) = &checkin.related_play_session_id {
                play_sessions::fulfill_next_schedule_slot(&conn, play_session_id, &checkin.id)
                    .map_err(|_| INTERNAL_ERROR)?;
            }
            Ok((checkin, alert_id, user.user_id.clone()))
        })
        .await
        .map_err(|_| INTERNAL_ERROR)??;

    notify_checkin_submitted(&pool, &checkin, &keyholder_id, alert_id.as_deref()).await;
    let response: CheckinResponse = checkin.into();
    publish_checkin_update(&streams, &response);
    Ok(Json(response))
}

async fn create_own(
    State(pool): State<Pool>,
    State(BlobDir(blob_dir)): State<BlobDir>,
    State(streams): State<PlaySessionStreams>,
    user: CurrentUser,
    multipart: Multipart,
) -> Result<Json<CheckinResponse>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    let parsed = parse_checkin_multipart(multipart).await?;
    if !valid_color(&parsed.color) {
        return Err(BAD_REQUEST);
    }
    let pool2 = pool.clone();
    let (checkin, alert_id, keyholder_id) =
        tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
            let mut conn = pool2.get().map_err(|_| INTERNAL_ERROR)?;
            let link_id = links::active_link_for_submissive(&conn, &user.user_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
            let (id, alert_id) = checkins::create_checkin(
                &mut conn,
                checkins::NewCheckin {
                    link_id: &link_id,
                    template_id: &parsed.template_id,
                    color: &parsed.color,
                    field_values: &parsed.field_values,
                    related_confinement_session_id: parsed.related_confinement_session_id.as_deref(),
                    related_assignment_id: parsed.related_assignment_id.as_deref(),
                    related_play_session_id: parsed.related_play_session_id.as_deref(),
                    created_by_user_id: &user.user_id,
                    has_photo: parsed.photo.is_some(),
                    has_audio: parsed.audio.is_some(),
                },
                &user.user_id,
            )
            .map_err(|e| match e {
                checkins::CreateCheckinError::TemplateNotFound => NOT_FOUND,
                checkins::CreateCheckinError::MissingRequiredField => BAD_REQUEST,
                checkins::CreateCheckinError::Db(_) => INTERNAL_ERROR,
            })?;
            if let Some((content_type, bytes)) = &parsed.photo {
                let stored = crate::storage::store(&blob_dir, content_type, bytes)
                    .map_err(|_| BAD_PHOTO)?;
                checkins::set_photo(&conn, &id, &stored.storage_path, content_type)
                    .map_err(|_| INTERNAL_ERROR)?;
            }
            if let Some((content_type, bytes)) = &parsed.audio {
                let stored = crate::storage::store(&blob_dir, content_type, bytes)
                    .map_err(|_| BAD_AUDIO)?;
                checkins::set_audio(&conn, &id, &stored.storage_path, content_type)
                    .map_err(|_| INTERNAL_ERROR)?;
            }
            let checkin = checkins::get_checkin(&conn, &id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(INTERNAL_ERROR)?;
            if let Some(play_session_id) = &checkin.related_play_session_id {
                play_sessions::fulfill_next_schedule_slot(&conn, play_session_id, &checkin.id)
                    .map_err(|_| INTERNAL_ERROR)?;
            }
            let (keyholder_id, _) = links::parties(&conn, &link_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(INTERNAL_ERROR)?;
            Ok((checkin, alert_id, keyholder_id))
        })
        .await
        .map_err(|_| INTERNAL_ERROR)??;

    notify_checkin_submitted(&pool, &checkin, &keyholder_id, alert_id.as_deref()).await;
    let response: CheckinResponse = checkin.into();
    publish_checkin_update(&streams, &response);
    Ok(Json(response))
}

/// Fans out a created/updated check-in over the live-session SSE
/// stream (13-checkins.md §5) when it's attached to a play session —
/// a no-op if nobody has that stream open right now.
fn publish_checkin_update(streams: &PlaySessionStreams, response: &CheckinResponse) {
    if let Some(play_session_id) = &response.related_play_session_id
        && let Ok(payload) = serde_json::to_string(response)
    {
        streams.publish_checkin(play_session_id, payload);
    }
}

/// 09-notifications.md §3: `checkin.submitted` (push only if red),
/// unless the template auto-escalated — in which case the alert's own
/// `safety.alert_raised` notification covers it and this doesn't also
/// send `checkin.red_flag` for the same event.
async fn notify_checkin_submitted(
    pool: &Pool,
    checkin: &checkins::Checkin,
    keyholder_id: &str,
    alert_id: Option<&str>,
) {
    if let Some(alert_id) = alert_id {
        let _ = notify::notify(
            pool,
            notify::Event {
                user_id: keyholder_id,
                link_id: Some(&checkin.link_id),
                notification_type: "safety.alert_raised",
                title: "Safety alert raised",
                body: Some("Auto-raised from a RED check-in."),
                link_path: Some("/keyholder/safety-alerts"),
                related_entity_type: Some("safety_alerts"),
                related_entity_id: Some(alert_id),
                push: true,
            },
        )
        .await;
        return;
    }
    let notification_type = if checkin.color == "red" {
        "checkin.red_flag"
    } else {
        "checkin.submitted"
    };
    let _ = notify::notify(
        pool,
        notify::Event {
            user_id: keyholder_id,
            link_id: Some(&checkin.link_id),
            notification_type,
            title: "Check-in submitted",
            body: None,
            link_path: None,
            related_entity_type: Some("checkins"),
            related_entity_id: Some(&checkin.id),
            push: checkin.color == "red",
        },
    )
    .await;
}

#[derive(Deserialize)]
struct ListCheckinsQuery {
    color: Option<String>,
    related_assignment_id: Option<String>,
    related_confinement_session_id: Option<String>,
    related_play_session_id: Option<String>,
}

async fn list_for_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Query(q): Query<ListCheckinsQuery>,
) -> Result<Json<Vec<CheckinResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id =
            links::active_or_paused_link_for_keyholder(&conn, &user.user_id, &submissive_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
        let list = checkins::list_for_link(
            &conn,
            &link_id,
            checkins::CheckinFilter {
                color: q.color.as_deref(),
                related_assignment_id: q.related_assignment_id.as_deref(),
                related_confinement_session_id: q.related_confinement_session_id.as_deref(),
                related_play_session_id: q.related_play_session_id.as_deref(),
            },
        )
        .map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn list_own(
    State(pool): State<Pool>,
    user: CurrentUser,
    Query(q): Query<ListCheckinsQuery>,
) -> Result<Json<Vec<CheckinResponse>>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = links::active_link_for_submissive(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let list = checkins::list_for_link(
            &conn,
            &link_id,
            checkins::CheckinFilter {
                color: q.color.as_deref(),
                related_assignment_id: q.related_assignment_id.as_deref(),
                related_confinement_session_id: q.related_confinement_session_id.as_deref(),
                related_play_session_id: q.related_play_session_id.as_deref(),
            },
        )
        .map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
struct PatchCheckinRequest {
    color: Option<String>,
    field_values: Option<serde_json::Value>,
}

fn require_reachable_checkin(
    conn: &rusqlite::Connection,
    user: &CurrentUser,
    checkin: &checkins::Checkin,
) -> Result<String, ApiError> {
    let (keyholder_id, submissive_id) = links::parties(conn, &checkin.link_id)
        .map_err(|_| INTERNAL_ERROR)?
        .ok_or(INTERNAL_ERROR)?;
    match user.role {
        Role::Keyholder if user.user_id == keyholder_id => Ok(submissive_id),
        Role::Submissive if user.user_id == submissive_id => Ok(submissive_id),
        _ => Err(NOT_FOUND),
    }
}

async fn patch_checkin(
    State(pool): State<Pool>,
    State(streams): State<PlaySessionStreams>,
    user: CurrentUser,
    Path(id): Path<String>,
    Json(req): Json<PatchCheckinRequest>,
) -> Result<StatusCode, ApiError> {
    if let Some(color) = &req.color
        && !valid_color(color)
    {
        return Err(BAD_REQUEST);
    }
    let pool2 = pool.clone();
    let id2 = id.clone();
    let (alert_id, was_red, updated, keyholder_id) =
        tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
            let mut conn = pool2.get().map_err(|_| INTERNAL_ERROR)?;
            let before = checkins::get_checkin(&conn, &id2)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
            let submissive_id = require_reachable_checkin(&conn, &user, &before)?;
            let was_red = before.color == "red";
            let field_values = req.field_values.as_ref().map(|v| v.to_string());
            let alert_id = checkins::update_checkin(
                &mut conn,
                &id2,
                checkins::CheckinEdit {
                    color: req.color.as_deref(),
                    field_values: field_values.as_deref(),
                },
                &submissive_id,
                &user.user_id,
            )
            .map_err(|e| match e {
                checkins::UpdateCheckinError::NotFound => NOT_FOUND,
                checkins::UpdateCheckinError::Db(_) => INTERNAL_ERROR,
            })?;
            let updated = checkins::get_checkin(&conn, &id2)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(INTERNAL_ERROR)?;
            let (keyholder_id, _) = links::parties(&conn, &updated.link_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(INTERNAL_ERROR)?;
            Ok((alert_id, was_red, updated, keyholder_id))
        })
        .await
        .map_err(|_| INTERNAL_ERROR)??;

    if let Some(alert_id) = alert_id {
        let _ = notify::notify(
            &pool,
            notify::Event {
                user_id: &keyholder_id,
                link_id: Some(&updated.link_id),
                notification_type: "safety.alert_raised",
                title: "Safety alert raised",
                body: Some("Auto-raised from a RED check-in."),
                link_path: Some("/keyholder/safety-alerts"),
                related_entity_type: Some("safety_alerts"),
                related_entity_id: Some(&alert_id),
                push: true,
            },
        )
        .await;
    } else if updated.color == "red" && !was_red {
        // Transitioned into red without auto-escalation configured —
        // the strong `checkin.red_flag` push (09-notifications.md §3),
        // same dedupe posture as the alert path: a follow-up edit that
        // stays red doesn't re-fire this.
        let _ = notify::notify(
            &pool,
            notify::Event {
                user_id: &keyholder_id,
                link_id: Some(&updated.link_id),
                notification_type: "checkin.red_flag",
                title: "Check-in flagged RED",
                body: None,
                link_path: None,
                related_entity_type: Some("checkins"),
                related_entity_id: Some(&updated.id),
                push: true,
            },
        )
        .await;
    }

    publish_checkin_update(&streams, &updated.into());
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /checkins/{id}/photo` — either party, on their own link. Optional
/// attachment, set independently of create/`PATCH` so the color/fields
/// request stays plain JSON (13-checkins.md doesn't require the photo at
/// creation time) — same division of labor as toy photos.
async fn upload_photo(
    State(pool): State<Pool>,
    State(BlobDir(blob_dir)): State<BlobDir>,
    State(streams): State<PlaySessionStreams>,
    user: CurrentUser,
    Path(id): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<CheckinResponse>, ApiError> {
    let mut photo: Option<(String, Vec<u8>)> = None;
    while let Some(field) = multipart.next_field().await.map_err(|_| BAD_PHOTO)? {
        if field.name() != Some("photo") {
            let _ = field.bytes().await;
            continue;
        }
        let content_type = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = field.bytes().await.map_err(|_| BAD_PHOTO)?.to_vec();
        photo = Some((content_type, bytes));
    }
    let (content_type, bytes) = photo.ok_or(BAD_PHOTO)?;
    if !valid_photo_content_type(&content_type) {
        return Err(BAD_PHOTO);
    }

    let updated = tokio::task::spawn_blocking(move || -> Result<checkins::Checkin, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let checkin = checkins::get_checkin(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        require_reachable_checkin(&conn, &user, &checkin)?;

        let stored =
            crate::storage::store(&blob_dir, &content_type, &bytes).map_err(|_| BAD_PHOTO)?;
        checkins::set_photo(&conn, &id, &stored.storage_path, &content_type)
            .map_err(|_| INTERNAL_ERROR)?;
        checkins::get_checkin(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    let response: CheckinResponse = updated.into();
    publish_checkin_update(&streams, &response);
    Ok(Json(response))
}

async fn download_photo(
    State(pool): State<Pool>,
    State(BlobDir(blob_dir)): State<BlobDir>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<Response, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let checkin = checkins::get_checkin(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        require_reachable_checkin(&conn, &user, &checkin)?;
        let (Some(path), Some(mime)) = (&checkin.photo_attachment_path, &checkin.photo_mime_type)
        else {
            return Err(NOT_FOUND);
        };
        let bytes = crate::storage::read(&blob_dir, path).map_err(|_| INTERNAL_ERROR)?;
        Ok(([(header::CONTENT_TYPE, mime.clone())], bytes).into_response())
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn delete_photo(
    State(pool): State<Pool>,
    State(streams): State<PlaySessionStreams>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let updated = tokio::task::spawn_blocking(move || -> Result<checkins::Checkin, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let checkin = checkins::get_checkin(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        require_reachable_checkin(&conn, &user, &checkin)?;
        checkins::clear_photo(&conn, &id).map_err(|_| INTERNAL_ERROR)?;
        checkins::get_checkin(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    publish_checkin_update(&streams, &updated.into());
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /checkins/{id}/audio` — mirrors `upload_photo` exactly, for the
/// independent voice-memo attachment slot.
async fn upload_audio(
    State(pool): State<Pool>,
    State(BlobDir(blob_dir)): State<BlobDir>,
    State(streams): State<PlaySessionStreams>,
    user: CurrentUser,
    Path(id): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<CheckinResponse>, ApiError> {
    let mut audio: Option<(String, Vec<u8>)> = None;
    while let Some(field) = multipart.next_field().await.map_err(|_| BAD_AUDIO)? {
        if field.name() != Some("audio") {
            let _ = field.bytes().await;
            continue;
        }
        let content_type = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = field.bytes().await.map_err(|_| BAD_AUDIO)?.to_vec();
        audio = Some((content_type, bytes));
    }
    let (content_type, bytes) = audio.ok_or(BAD_AUDIO)?;
    if !valid_audio_content_type(&content_type) {
        return Err(BAD_AUDIO);
    }

    let updated = tokio::task::spawn_blocking(move || -> Result<checkins::Checkin, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let checkin = checkins::get_checkin(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        require_reachable_checkin(&conn, &user, &checkin)?;

        let stored =
            crate::storage::store(&blob_dir, &content_type, &bytes).map_err(|_| BAD_AUDIO)?;
        checkins::set_audio(&conn, &id, &stored.storage_path, &content_type)
            .map_err(|_| INTERNAL_ERROR)?;
        checkins::get_checkin(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    let response: CheckinResponse = updated.into();
    publish_checkin_update(&streams, &response);
    Ok(Json(response))
}

async fn download_audio(
    State(pool): State<Pool>,
    State(BlobDir(blob_dir)): State<BlobDir>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<Response, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let checkin = checkins::get_checkin(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        require_reachable_checkin(&conn, &user, &checkin)?;
        let (Some(path), Some(mime)) = (&checkin.audio_attachment_path, &checkin.audio_mime_type)
        else {
            return Err(NOT_FOUND);
        };
        let bytes = crate::storage::read(&blob_dir, path).map_err(|_| INTERNAL_ERROR)?;
        Ok(([(header::CONTENT_TYPE, mime.clone())], bytes).into_response())
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn delete_audio(
    State(pool): State<Pool>,
    State(streams): State<PlaySessionStreams>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let updated = tokio::task::spawn_blocking(move || -> Result<checkins::Checkin, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let checkin = checkins::get_checkin(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        require_reachable_checkin(&conn, &user, &checkin)?;
        checkins::clear_audio(&conn, &id).map_err(|_| INTERNAL_ERROR)?;
        checkins::get_checkin(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    publish_checkin_update(&streams, &updated.into());
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route(
            "/keyholder/checkin-templates",
            get(list_templates).post(create_template),
        )
        .route("/keyholder/checkin-templates/{id}", patch(patch_template))
        .route(
            "/submissive/checkin-templates",
            get(list_templates_for_submissive),
        )
        .route(
            "/keyholder/submissives/{id}/checkins",
            get(list_for_keyholder).post(create_for_keyholder),
        )
        .route("/submissive/checkins", get(list_own).post(create_own))
        .route("/checkins/{id}", patch(patch_checkin))
        .route(
            "/checkins/{id}/photo",
            post(upload_photo).get(download_photo).delete(delete_photo),
        )
        .route(
            "/checkins/{id}/audio",
            post(upload_audio).get(download_audio).delete(delete_audio),
        )
        .layer(DefaultBodyLimit::max(MAX_CHECKIN_MEDIA_BYTES))
}
