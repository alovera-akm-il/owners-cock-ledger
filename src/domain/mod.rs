pub mod audit;
pub mod chastity;
pub mod invites;
pub mod links;
pub mod proofs;
pub mod safety;
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
