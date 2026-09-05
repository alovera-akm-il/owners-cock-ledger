//! Structured hard/soft limits (06-future-extensions.md §9): the
//! Keyholder-managed catalog (`limit_items`, global seed rows plus
//! their own additions) and a submissive's own per-item ratings
//! (`submissive_limit_ratings`). Same visibility split as the
//! existing free-text `hard_limits`/`soft_limits` fields
//! (`api::profiles`): a submissive's ratings are RW\* (self), a
//! Keyholder gets R\* on one submissive's ratings — rating a limit is
//! exactly as submissive-owned an act as writing the paragraph is.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::session::{CurrentUser, Role};
use crate::db::{self, Pool};
use crate::domain::{limits, links};

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found");
const BAD_REQUEST: ApiError =
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "malformed request");

fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Serialize)]
struct LimitItemResponse {
    id: String,
    category: String,
    label: String,
    description: Option<String>,
    active: bool,
    is_global: bool,
    created_at: String,
}

impl From<limits::LimitItem> for LimitItemResponse {
    fn from(i: limits::LimitItem) -> Self {
        Self {
            id: i.id,
            category: i.category,
            label: i.label,
            description: i.description,
            active: i.active,
            is_global: i.keyholder_id.is_none(),
            created_at: iso8601(i.created_at),
        }
    }
}

/// `GET /keyholder/limit-items` — the full catalog reachable by this
/// Keyholder: global seed items plus their own additions, active and
/// inactive alike (so an inactive one can be reactivated).
async fn list_items(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<LimitItemResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let list =
            limits::list_items_for_keyholder(&conn, &user.user_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
struct CreateItemRequest {
    category: String,
    label: String,
    description: Option<String>,
}

async fn create_item(
    State(pool): State<Pool>,
    user: CurrentUser,
    Json(req): Json<CreateItemRequest>,
) -> Result<Json<LimitItemResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    if req.category.trim().is_empty() || req.label.trim().is_empty() {
        return Err(BAD_REQUEST);
    }
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let id = limits::create_item(
            &conn,
            limits::NewItem {
                keyholder_id: &user.user_id,
                category: &req.category,
                label: &req.label,
                description: req.description.as_deref(),
            },
        )
        .map_err(|_| INTERNAL_ERROR)?;
        let item = limits::get_item(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)?;
        Ok(Json(item.into()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize, Default)]
struct PatchItemRequest {
    category: Option<String>,
    label: Option<String>,
    #[serde(default, deserialize_with = "deserialize_some")]
    description: Option<Option<String>>,
    active: Option<bool>,
}

/// `PATCH /keyholder/limit-items/{id}` — a Keyholder may only edit
/// their own additions; a global seed item or another Keyholder's
/// returns `404`, same posture as everywhere else in this API.
async fn patch_item(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
    Json(req): Json<PatchItemRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let updated = limits::update_item(
            &conn,
            &id,
            &user.user_id,
            limits::ItemEdit {
                category: req.category.as_deref(),
                label: req.label.as_deref(),
                description: req.description.as_ref().map(|v| v.as_deref()),
                active: req.active,
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
struct ItemWithRatingResponse {
    id: String,
    category: String,
    label: String,
    description: Option<String>,
    rating: Option<String>,
    notes: Option<String>,
    rated_at: Option<String>,
}

fn item_with_rating_response(
    item: limits::LimitItem,
    rating: Option<limits::Rating>,
) -> ItemWithRatingResponse {
    ItemWithRatingResponse {
        id: item.id,
        category: item.category,
        label: item.label,
        description: item.description,
        rating: rating.as_ref().map(|r| r.rating.clone()),
        notes: rating.as_ref().and_then(|r| r.notes.clone()),
        rated_at: rating.map(|r| iso8601(r.updated_at)),
    }
}

/// `GET /submissive/limit-items` — the catalog reachable by this
/// submissive's own Keyholder (active items only), paired with their
/// own rating of each if they've given one. Not gated by
/// `catalog_visible_to_submissive` — unlike the task/reward catalog,
/// a submissive needs to see and rate this regardless of that
/// setting, since it's about their own safety, not the Keyholder's
/// authored catalog.
async fn list_own_items(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<ItemWithRatingResponse>>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = links::active_link_for_submissive(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let (keyholder_id, _) = links::parties(&conn, &link_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)?;
        let list =
            limits::list_items_with_ratings_for_submissive(&conn, &keyholder_id, &user.user_id)
                .map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(
            list.into_iter()
                .map(|(item, rating)| item_with_rating_response(item, rating))
                .collect(),
        ))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
struct SetRatingRequest {
    rating: String,
    notes: Option<String>,
}

async fn set_own_rating(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(item_id): Path<String>,
    Json(req): Json<SetRatingRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        limits::set_rating(
            &conn,
            &user.user_id,
            &item_id,
            &req.rating,
            req.notes.as_deref(),
        )
        .map_err(|e| match e {
            limits::SetRatingError::InvalidRating => BAD_REQUEST,
            limits::SetRatingError::ItemNotFound => NOT_FOUND,
            limits::SetRatingError::Db(_) => INTERNAL_ERROR,
        })?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

/// `DELETE /submissive/limit-ratings/{item_id}` — back to "not
/// discussed," not to some default value.
async fn clear_own_rating(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(item_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        limits::clear_rating(&conn, &user.user_id, &item_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

/// `GET /keyholder/limit-ratings` — a Keyholder's own catalog paired
/// with their own rating of each, mirroring `list_own_items` on the
/// submissive side.
async fn list_own_items_for_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<ItemWithRatingResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let list = limits::list_items_with_ratings_for_keyholder(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(
            list.into_iter()
                .map(|(item, rating)| item_with_keyholder_rating_response(item, rating))
                .collect(),
        ))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

fn item_with_keyholder_rating_response(
    item: limits::LimitItem,
    rating: Option<limits::KeyholderRating>,
) -> ItemWithRatingResponse {
    ItemWithRatingResponse {
        id: item.id,
        category: item.category,
        label: item.label,
        description: item.description,
        rating: rating.as_ref().map(|r| r.rating.clone()),
        notes: rating.as_ref().and_then(|r| r.notes.clone()),
        rated_at: rating.map(|r| iso8601(r.updated_at)),
    }
}

async fn set_own_rating_for_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(item_id): Path<String>,
    Json(req): Json<SetRatingRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        limits::set_keyholder_rating(
            &conn,
            &user.user_id,
            &item_id,
            &req.rating,
            req.notes.as_deref(),
        )
        .map_err(|e| match e {
            limits::SetRatingError::InvalidRating => BAD_REQUEST,
            limits::SetRatingError::ItemNotFound => NOT_FOUND,
            limits::SetRatingError::Db(_) => INTERNAL_ERROR,
        })?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

/// `DELETE /keyholder/limit-ratings/{item_id}` — back to "not
/// discussed," not to some default value.
async fn clear_own_rating_for_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(item_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        limits::clear_keyholder_rating(&conn, &user.user_id, &item_id)
            .map_err(|_| INTERNAL_ERROR)?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

/// `GET /keyholder/submissives/{id}/limit-ratings` — read-only, same
/// visibility as the free-text limits fields on the submissive detail
/// page.
async fn list_ratings_for_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
) -> Result<Json<Vec<ItemWithRatingResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        links::active_or_paused_link_for_keyholder(&conn, &user.user_id, &submissive_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let list =
            limits::list_items_with_ratings_for_submissive(&conn, &user.user_id, &submissive_id)
                .map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(
            list.into_iter()
                .map(|(item, rating)| item_with_rating_response(item, rating))
                .collect(),
        ))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route("/keyholder/limit-items", get(list_items).post(create_item))
        .route("/keyholder/limit-items/{id}", patch(patch_item))
        .route("/submissive/limit-items", get(list_own_items))
        .route(
            "/submissive/limit-ratings/{item_id}",
            put(set_own_rating).delete(clear_own_rating),
        )
        .route(
            "/keyholder/limit-ratings",
            get(list_own_items_for_keyholder),
        )
        .route(
            "/keyholder/limit-ratings/{item_id}",
            put(set_own_rating_for_keyholder).delete(clear_own_rating_for_keyholder),
        )
        .route(
            "/keyholder/submissives/{id}/limit-ratings",
            get(list_ratings_for_keyholder),
        )
}
