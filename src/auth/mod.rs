pub mod csrf;
pub mod password;
pub mod session;
pub mod token;

/// Whether the session and CSRF cookies should carry the `Secure` flag.
/// Defaults to `true` (required for any real deployment, per
/// `05-security-and-privacy.md` §2) — a browser silently refuses to
/// store a `Secure` cookie on a plain-HTTP origin, so a self-hosted
/// instance reached over `http://` (e.g. a bare LAN IP with no reverse
/// proxy in front) would otherwise be unable to complete login at all.
/// Setting `INSECURE_COOKIES=1` opts out for that specific case; leave
/// it unset for any deployment reachable over the public internet.
pub fn cookies_secure() -> bool {
    std::env::var("INSECURE_COOKIES").as_deref() != Ok("1")
}
