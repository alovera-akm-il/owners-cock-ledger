//! Personal profile fields — distinct from account credentials (§1) and,
//! for the Keyholder, API tokens (§12) (03-api-design.md §3).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR};
use crate::auth::session::{CurrentUser, Role};
use crate::db::{self, Pool};
use crate::domain::{links, profiles};

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found");

/// Distinguishes "field omitted" from "field explicitly cleared" — see
/// the identical helper in `api::templates` for the full reasoning.
fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Serialize)]
struct KeyholderProfileResponse {
    bio: Option<String>,
    contact_info: Option<String>,
    timezone: Option<String>,
    hard_limits: Option<String>,
    soft_limits: Option<String>,
}

impl From<profiles::KeyholderProfile> for KeyholderProfileResponse {
    fn from(p: profiles::KeyholderProfile) -> Self {
        Self {
            bio: p.bio,
            contact_info: p.contact_info,
            timezone: p.timezone,
            hard_limits: p.hard_limits,
            soft_limits: p.soft_limits,
        }
    }
}

#[derive(Serialize)]
struct SubmissiveProfileResponse {
    bio: Option<String>,
    safeword: Option<String>,
    hard_limits: Option<String>,
    soft_limits: Option<String>,
    emergency_contact: Option<String>,
    timezone: Option<String>,
    // Only ever populated for the Keyholder-viewing-a-submissive path
    // (`profile_for_keyholder`) — a submissive's own `GET /profile`
    // never sets this (01-data-model.md §2).
    #[serde(skip_serializing_if = "Option::is_none")]
    keyholder_notes: Option<String>,
}

impl From<profiles::SubmissiveProfile> for SubmissiveProfileResponse {
    fn from(p: profiles::SubmissiveProfile) -> Self {
        Self {
            bio: p.bio,
            safeword: p.safeword,
            hard_limits: p.hard_limits,
            soft_limits: p.soft_limits,
            emergency_contact: p.emergency_contact,
            timezone: p.timezone,
            keyholder_notes: None,
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum ProfileResponse {
    Keyholder(KeyholderProfileResponse),
    Submissive(SubmissiveProfileResponse),
}

/// `GET /profile` — own profile, role-appropriate shape.
async fn own_profile(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<ProfileResponse>, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<ProfileResponse, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        match user.role {
            Role::Keyholder => {
                let p = profiles::get_keyholder_profile(&conn, &user.user_id)
                    .map_err(|_| INTERNAL_ERROR)?;
                Ok(ProfileResponse::Keyholder(p.into()))
            }
            Role::Submissive => {
                let p = profiles::get_submissive_profile(&conn, &user.user_id)
                    .map_err(|_| INTERNAL_ERROR)?;
                Ok(ProfileResponse::Submissive(p.into()))
            }
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map(Json)
}

#[derive(Deserialize, Default)]
struct PatchOwnProfileRequest {
    #[serde(default, deserialize_with = "deserialize_some")]
    bio: Option<Option<String>>,
    // Keyholder-only field; ignored (not an error) when sent by a
    // submissive, same posture as the rest of this endpoint quietly
    // only ever touching the fields the caller's role owns.
    #[serde(default, deserialize_with = "deserialize_some")]
    contact_info: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    timezone: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    hard_limits: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    soft_limits: Option<Option<String>>,
    // Submissive-only field; ignored when sent by a keyholder.
    #[serde(default, deserialize_with = "deserialize_some")]
    safeword: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    emergency_contact: Option<Option<String>>,
}

/// `PATCH /profile` — a submissive can edit `bio`/`safeword`/
/// `hard_limits`/`soft_limits`/`emergency_contact`/`timezone`; a
/// Keyholder can edit `bio`/`contact_info`/`hard_limits`/`soft_limits`/
/// `timezone`. Fields the caller's role doesn't own are silently
/// ignored rather than erroring, so one request shape works for both.
async fn patch_own_profile(
    State(pool): State<Pool>,
    user: CurrentUser,
    Json(req): Json<PatchOwnProfileRequest>,
) -> Result<StatusCode, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        match user.role {
            Role::Keyholder => {
                profiles::update_keyholder_profile(
                    &conn,
                    &user.user_id,
                    profiles::KeyholderProfileEdit {
                        bio: req.bio.as_ref().map(|v| v.as_deref()),
                        contact_info: req.contact_info.as_ref().map(|v| v.as_deref()),
                        timezone: req.timezone.as_ref().map(|v| v.as_deref()),
                        hard_limits: req.hard_limits.as_ref().map(|v| v.as_deref()),
                        soft_limits: req.soft_limits.as_ref().map(|v| v.as_deref()),
                    },
                )
                .map_err(|_| INTERNAL_ERROR)?;
            }
            Role::Submissive => {
                profiles::update_submissive_profile(
                    &conn,
                    &user.user_id,
                    profiles::SubmissiveProfileEdit {
                        bio: req.bio.as_ref().map(|v| v.as_deref()),
                        safeword: req.safeword.as_ref().map(|v| v.as_deref()),
                        hard_limits: req.hard_limits.as_ref().map(|v| v.as_deref()),
                        soft_limits: req.soft_limits.as_ref().map(|v| v.as_deref()),
                        emergency_contact: req.emergency_contact.as_ref().map(|v| v.as_deref()),
                        timezone: req.timezone.as_ref().map(|v| v.as_deref()),
                    },
                )
                .map_err(|_| INTERNAL_ERROR)?;
            }
        }
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

/// `GET /keyholder/submissives/{id}/profile` — includes fields never
/// exposed to a different Keyholder or to any submissive other than the
/// profile's owner.
async fn profile_for_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
) -> Result<Json<SubmissiveProfileResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:submissives")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<SubmissiveProfileResponse, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        links::active_or_paused_link_for_keyholder(&conn, &user.user_id, &submissive_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let p =
            profiles::get_submissive_profile(&conn, &submissive_id).map_err(|_| INTERNAL_ERROR)?;
        // `From<profiles::SubmissiveProfile>` always zeroes
        // keyholder_notes (it's the shape a submissive's own `GET
        // /profile` uses too) — restore it here, the one path allowed
        // to see it.
        let keyholder_notes = p.keyholder_notes.clone();
        let mut response: SubmissiveProfileResponse = p.into();
        response.keyholder_notes = keyholder_notes;
        Ok(response)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map(Json)
}

/// `GET /submissive/keyholder-profile` — read-only mirror of the linked
/// Keyholder's stated boundaries, the submissive-facing counterpart to
/// `profile_for_keyholder` (mockup's `submissive-profile.html` "Your
/// Keyholder's boundaries" panel, §16 of `16-mockup-implementation-gaps.md`).
async fn keyholder_profile_for_submissive(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<KeyholderProfileResponse>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<KeyholderProfileResponse, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = links::active_or_paused_link_for_submissive(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let (keyholder_id, _) = links::parties(&conn, &link_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let p =
            profiles::get_keyholder_profile(&conn, &keyholder_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(p.into())
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map(Json)
}

#[derive(Deserialize)]
struct PatchKeyholderNotesRequest {
    keyholder_notes: Option<String>,
}

/// `PATCH /keyholder/submissives/{id}/profile/notes` — the one field
/// only the Keyholder can write on the submissive's profile.
async fn patch_keyholder_notes(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Json(req): Json<PatchKeyholderNotesRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:submissives")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        links::active_or_paused_link_for_keyholder(&conn, &user.user_id, &submissive_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        profiles::update_keyholder_notes(&conn, &submissive_id, req.keyholder_notes.as_deref())
            .map_err(|_| INTERNAL_ERROR)?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route("/profile", get(own_profile).patch(patch_own_profile))
        .route(
            "/submissive/keyholder-profile",
            get(keyholder_profile_for_submissive),
        )
        .route(
            "/keyholder/submissives/{id}/profile",
            get(profile_for_keyholder),
        )
        .route(
            "/keyholder/submissives/{id}/profile/notes",
            patch(patch_keyholder_notes),
        )
}
