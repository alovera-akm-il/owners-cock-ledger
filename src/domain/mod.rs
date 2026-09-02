pub mod api_tokens;
pub mod audit;
pub mod chastity;
pub mod checkins;
pub mod invites;
pub mod limits;
pub mod links;
pub mod notifications;
pub mod password_reset;
pub mod play_sessions;
pub mod points;
pub mod profiles;
pub mod proofs;
pub mod push;
pub mod rewards_punishments;
pub mod safety;
pub mod toys;
pub mod two_factor;
pub mod users;
pub mod verification;

/// Shared across every domain module that needs to distinguish "this
/// exact row already exists / a partial-unique index was violated" from
/// an ordinary DB error — e.g. a duplicate email, or a second open
/// confinement session.
pub(crate) fn is_unique_violation(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(e, _) if e.code == rusqlite::ErrorCode::ConstraintViolation
    )
}
