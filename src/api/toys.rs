//! Toy catalog (03-api-design.md §10a). Per-submissive, either role
//! may create/edit; retiring is Keyholder-only.

use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::session::{CurrentUser, Role};
use crate::db::{self, BlobDir, Pool};
use crate::domain::{links, toys};
use crate::notify;

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found");
const CONFLICT: ApiError = ApiError::new(
    StatusCode::CONFLICT,
    "conflict",
    "request conflicts with current state",
);
const BAD_REQUEST: ApiError = ApiError::new(
    StatusCode::BAD_REQUEST,
    "bad_request",
    "expected a single 'photo' field with an image/jpeg or image/png file",
);
const MAX_PHOTO_UPLOAD_BYTES: usize = 10 * 1024 * 1024;

fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Serialize)]
struct ToyResponse {
    id: String,
    submissive_id: String,
    added_by_user_id: String,
    name: String,
    category: Option<String>,
    material: Option<String>,
    brand: Option<String>,
    size_notes: Option<String>,
    color: Option<String>,
    compatible_device_id: Option<String>,
    storage_location: Option<String>,
    care_instructions: Option<String>,
    usage_notes: Option<String>,
    tags: Option<serde_json::Value>,
    photo_url: Option<String>,
    acquired_at: Option<String>,
    retirement_requested_at: Option<String>,
    retired_at: Option<String>,
    retired_by_user_id: Option<String>,
}

impl From<toys::Toy> for ToyResponse {
    fn from(t: toys::Toy) -> Self {
        let photo_url = t
            .photo_attachment_path
            .is_some()
            .then(|| format!("/api/v1/toys/{}/photo", t.id));
        Self {
            id: t.id,
            submissive_id: t.submissive_id,
            added_by_user_id: t.added_by_user_id,
            name: t.name,
            category: t.category,
            material: t.material,
            brand: t.brand,
            size_notes: t.size_notes,
            color: t.color,
            compatible_device_id: t.compatible_device_id,
            storage_location: t.storage_location,
            care_instructions: t.care_instructions,
            usage_notes: t.usage_notes,
            tags: t.tags.as_deref().and_then(|s| serde_json::from_str(s).ok()),
            photo_url,
            acquired_at: t.acquired_at.map(iso8601),
            retirement_requested_at: t.retirement_requested_at.map(iso8601),
            retired_at: t.retired_at.map(iso8601),
            retired_by_user_id: t.retired_by_user_id,
        }
    }
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    include_retired: bool,
}

async fn list_for_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<ToyResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:submissives")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        links::active_or_paused_link_for_keyholder(&conn, &user.user_id, &submissive_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let list = toys::list_for_submissive(&conn, &submissive_id, q.include_retired)
            .map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn list_own(
    State(pool): State<Pool>,
    user: CurrentUser,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<ToyResponse>>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let list = toys::list_for_submissive(&conn, &user.user_id, q.include_retired)
            .map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
struct CreateToyRequest {
    name: String,
    category: Option<String>,
    material: Option<String>,
    brand: Option<String>,
    size_notes: Option<String>,
    color: Option<String>,
    compatible_device_id: Option<String>,
    storage_location: Option<String>,
    care_instructions: Option<String>,
    usage_notes: Option<String>,
    tags: Option<serde_json::Value>,
    acquired_at: Option<String>,
}

fn parse_iso8601(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
}

async fn create_for_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Json(req): Json<CreateToyRequest>,
) -> Result<Json<ToyResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:chastity")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        links::active_or_paused_link_for_keyholder(&conn, &user.user_id, &submissive_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let id = create_toy(&conn, &submissive_id, &user.user_id, &req)?;
        let t = toys::get(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)?;
        Ok(Json(t.into()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn create_own(
    State(pool): State<Pool>,
    user: CurrentUser,
    Json(req): Json<CreateToyRequest>,
) -> Result<Json<ToyResponse>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let id = create_toy(&conn, &user.user_id, &user.user_id, &req)?;
        let t = toys::get(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)?;
        Ok(Json(t.into()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

fn create_toy(
    conn: &rusqlite::Connection,
    submissive_id: &str,
    added_by_user_id: &str,
    req: &CreateToyRequest,
) -> Result<String, ApiError> {
    let tags = req.tags.as_ref().map(|v| v.to_string());
    let acquired_at = req.acquired_at.as_deref().and_then(parse_iso8601);
    toys::create(
        conn,
        toys::NewToy {
            submissive_id,
            added_by_user_id,
            name: &req.name,
            category: req.category.as_deref(),
            material: req.material.as_deref(),
            brand: req.brand.as_deref(),
            size_notes: req.size_notes.as_deref(),
            color: req.color.as_deref(),
            compatible_device_id: req.compatible_device_id.as_deref(),
            storage_location: req.storage_location.as_deref(),
            care_instructions: req.care_instructions.as_deref(),
            usage_notes: req.usage_notes.as_deref(),
            tags: tags.as_deref(),
            photo_attachment_path: None,
            acquired_at,
        },
    )
    .map_err(|_| INTERNAL_ERROR)
}

#[derive(Deserialize, Default)]
struct PatchToyRequest {
    name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_some")]
    category: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    material: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    brand: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    size_notes: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    color: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    compatible_device_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    storage_location: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    care_instructions: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    usage_notes: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    tags: Option<Option<serde_json::Value>>,
}

/// Ownership check shared by every `.../toys/{id}` route: a Keyholder
/// must own the link to the toy's submissive; a submissive must own
/// the toy itself. 404, not 403, on mismatch — same "don't confirm
/// another Keyholder's submissive/toy exists" posture as the rest of
/// this API.
fn require_reachable_toy(
    conn: &rusqlite::Connection,
    user: &CurrentUser,
    toy: &toys::Toy,
) -> Result<(), ApiError> {
    match user.role {
        Role::Keyholder => {
            links::active_or_paused_link_for_keyholder(conn, &user.user_id, &toy.submissive_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
        }
        Role::Submissive => {
            if toy.submissive_id != user.user_id {
                return Err(NOT_FOUND);
            }
        }
    }
    Ok(())
}

async fn patch_toy(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
    Json(req): Json<PatchToyRequest>,
) -> Result<StatusCode, ApiError> {
    if user.role == Role::Keyholder {
        user.require_scope("manage:chastity")
            .map_err(|_| FORBIDDEN)?;
    }
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let toy = toys::get(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        require_reachable_toy(&conn, &user, &toy)?;

        let tags = req.tags.as_ref().map(|v| v.as_ref().map(|j| j.to_string()));
        let updated = toys::update(
            &conn,
            &id,
            toys::ToyEdit {
                name: req.name.as_deref(),
                category: req.category.as_ref().map(|v| v.as_deref()),
                material: req.material.as_ref().map(|v| v.as_deref()),
                brand: req.brand.as_ref().map(|v| v.as_deref()),
                size_notes: req.size_notes.as_ref().map(|v| v.as_deref()),
                color: req.color.as_ref().map(|v| v.as_deref()),
                compatible_device_id: req.compatible_device_id.as_ref().map(|v| v.as_deref()),
                storage_location: req.storage_location.as_ref().map(|v| v.as_deref()),
                care_instructions: req.care_instructions.as_ref().map(|v| v.as_deref()),
                usage_notes: req.usage_notes.as_ref().map(|v| v.as_deref()),
                tags: tags.as_ref().map(|v| v.as_deref()),
                photo_attachment_path: None,
                photo_mime_type: None,
                acquired_at: None,
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

async fn request_removal(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    let pool2 = pool.clone();
    let id2 = id.clone();
    let (keyholder_id, name) =
        tokio::task::spawn_blocking(move || -> Result<(String, String), ApiError> {
            let conn = pool2.get().map_err(|_| INTERNAL_ERROR)?;
            let toy = toys::get(&conn, &id2)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
            if toy.submissive_id != user.user_id {
                return Err(NOT_FOUND);
            }
            match toys::request_removal(&conn, &id2) {
                Ok(()) => {}
                Err(toys::RequestRemovalError::NotFound) => return Err(NOT_FOUND),
                Err(toys::RequestRemovalError::Conflict) => return Err(CONFLICT),
                Err(toys::RequestRemovalError::Db(_)) => return Err(INTERNAL_ERROR),
            }
            let (keyholder_id, _) = links::parties(
                &conn,
                &links::active_link_for_submissive(&conn, &user.user_id)
                    .map_err(|_| INTERNAL_ERROR)?
                    .ok_or(INTERNAL_ERROR)?,
            )
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)?;
            Ok((keyholder_id, toy.name))
        })
        .await
        .map_err(|_| INTERNAL_ERROR)??;

    let _ = notify::notify(
        &pool,
        notify::Event {
            user_id: &keyholder_id,
            link_id: None,
            notification_type: "toy.retirement_requested",
            title: &format!("Removal requested: {name}"),
            body: None,
            link_path: None,
            related_entity_type: Some("toys"),
            related_entity_id: Some(&id),
            push: false,
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

async fn retire(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:chastity")
        .map_err(|_| FORBIDDEN)?;
    let pool2 = pool.clone();
    let id2 = id.clone();
    let submissive_id = tokio::task::spawn_blocking(move || -> Result<String, ApiError> {
        let mut conn = pool2.get().map_err(|_| INTERNAL_ERROR)?;
        let toy = toys::get(&conn, &id2)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        require_reachable_toy(&conn, &user, &toy)?;
        toys::retire(&mut conn, &id2, &user.user_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(toy.submissive_id)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    let _ = notify::notify(
        &pool,
        notify::Event {
            user_id: &submissive_id,
            link_id: None,
            notification_type: "toy.retirement_resolved",
            title: "A toy was retired from your catalog",
            body: None,
            link_path: Some("/submissive/toys"),
            related_entity_type: Some("toys"),
            related_entity_id: Some(&id),
            push: false,
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

async fn decline_removal(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:chastity")
        .map_err(|_| FORBIDDEN)?;
    let pool2 = pool.clone();
    let id2 = id.clone();
    let submissive_id = tokio::task::spawn_blocking(move || -> Result<String, ApiError> {
        let mut conn = pool2.get().map_err(|_| INTERNAL_ERROR)?;
        let toy = toys::get(&conn, &id2)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        require_reachable_toy(&conn, &user, &toy)?;
        match toys::decline_removal(&mut conn, &id2, &user.user_id) {
            Ok(()) => Ok(toy.submissive_id),
            Err(toys::DeclineRemovalError::NotPending) => Err(CONFLICT),
            Err(toys::DeclineRemovalError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    let _ = notify::notify(
        &pool,
        notify::Event {
            user_id: &submissive_id,
            link_id: None,
            notification_type: "toy.retirement_resolved",
            title: "Your removal request was declined",
            body: None,
            link_path: Some("/submissive/toys"),
            related_entity_type: Some("toys"),
            related_entity_id: Some(&id),
            push: false,
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// `POST /toys/{id}/photo` — a single photo per toy, matching
/// `photo_attachment_path`'s existing singular shape rather than
/// introducing a multi-attachment table for one field. Either role may
/// upload, same posture as editing any other toy field
/// (12-toy-catalog.md §3).
async fn upload_photo(
    State(pool): State<Pool>,
    State(BlobDir(blob_dir)): State<BlobDir>,
    user: CurrentUser,
    Path(id): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<ToyResponse>, ApiError> {
    if user.role == Role::Keyholder {
        user.require_scope("manage:chastity")
            .map_err(|_| FORBIDDEN)?;
    }
    let mut photo: Option<(String, Vec<u8>)> = None;
    while let Some(field) = multipart.next_field().await.map_err(|_| BAD_REQUEST)? {
        if field.name() != Some("photo") {
            let _ = field.bytes().await;
            continue;
        }
        let content_type = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = field.bytes().await.map_err(|_| BAD_REQUEST)?.to_vec();
        photo = Some((content_type, bytes));
    }
    let (content_type, bytes) = photo.ok_or(BAD_REQUEST)?;
    if !matches!(content_type.as_str(), "image/jpeg" | "image/png") {
        return Err(BAD_REQUEST);
    }

    tokio::task::spawn_blocking(move || -> Result<Json<ToyResponse>, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let toy = toys::get(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        require_reachable_toy(&conn, &user, &toy)?;

        let stored =
            crate::storage::store(&blob_dir, &content_type, &bytes).map_err(|_| BAD_REQUEST)?;
        let updated = toys::update(
            &conn,
            &id,
            toys::ToyEdit {
                photo_attachment_path: Some(Some(&stored.storage_path)),
                photo_mime_type: Some(Some(&content_type)),
                ..Default::default()
            },
        )
        .map_err(|_| INTERNAL_ERROR)?;
        if !updated {
            return Err(NOT_FOUND);
        }
        let t = toys::get(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)?;
        Ok(Json(t.into()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn download_photo(
    State(pool): State<Pool>,
    State(BlobDir(blob_dir)): State<BlobDir>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    if user.role == Role::Keyholder {
        user.require_scope("read:submissives")
            .map_err(|_| FORBIDDEN)?;
    }
    tokio::task::spawn_blocking(move || -> Result<Response, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let toy = toys::get(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        require_reachable_toy(&conn, &user, &toy)?;
        let (Some(path), Some(mime)) = (&toy.photo_attachment_path, &toy.photo_mime_type) else {
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
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if user.role == Role::Keyholder {
        user.require_scope("manage:chastity")
            .map_err(|_| FORBIDDEN)?;
    }
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let toy = toys::get(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        require_reachable_toy(&conn, &user, &toy)?;
        toys::update(
            &conn,
            &id,
            toys::ToyEdit {
                photo_attachment_path: Some(None),
                photo_mime_type: Some(None),
                ..Default::default()
            },
        )
        .map_err(|_| INTERNAL_ERROR)?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route(
            "/keyholder/submissives/{id}/toys",
            get(list_for_keyholder).post(create_for_keyholder),
        )
        .route("/submissive/toys", get(list_own).post(create_own))
        .route("/toys/{id}", patch(patch_toy))
        .route(
            "/toys/{id}/photo",
            post(upload_photo).get(download_photo).delete(delete_photo),
        )
        .layer(DefaultBodyLimit::max(MAX_PHOTO_UPLOAD_BYTES))
        .route(
            "/submissive/toys/{id}/request-removal",
            post(request_removal),
        )
        .route("/keyholder/toys/{id}/retire", post(retire))
        .route(
            "/keyholder/toys/{id}/decline-removal",
            post(decline_removal),
        )
}
