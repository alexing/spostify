//! The name people see. This fork shows itself as Spostify, but keeps
//! upstream Spotifast's internal identifiers (crate and binary name, bundle
//! id, keychain service, settings and cache paths, ObjC class names) so it
//! rebases cheaply onto upstream and keeps the existing Spotify session and
//! saved keys.

/// The app's visible name, in titles, menus, the Connect device list and
/// every translated message.
pub const NAME: &str = "Spostify";

/// Upstream's visible name. Translated messages still spell it, and older
/// settings files saved it as the Connect device name.
pub const UPSTREAM_NAME: &str = "Spotifast";
