//! OpenFan profile schema, versioned migration and importers.
//!
//! # Status
//!
//! Phase 1 scaffold. The shape below fixes the two decisions that are expensive to
//! change later; the rest lands in Phase 5 (persistence) and Phase 7 (import).
//!
//! 1. **Profiles carry an explicit schema version and are migrated forward on load.**
//!    A profile is a user's tuning work, sometimes hours of it. Silently failing to load
//!    one after an update is not acceptable, so migration is in the design from the first
//!    commit rather than retrofitted.
//! 2. **The format is OS-neutral.** Channel and sensor references are stable logical ids
//!    rather than anything Windows-shaped, because sharing one profile across a dual-boot
//!    system is a goal (see `plans/open-fan.md`).

#![forbid(unsafe_code)]

pub mod presets;

use of_core::Graph;
use serde::{Deserialize, Serialize};

/// Schema version of profiles written by this build. Bump on every breaking change and
/// add the corresponding step to [`migrate`].
pub const CURRENT_SCHEMA: u32 = 1;

pub use presets::{ChannelSummary, HardwareSummary, Preset, SensorSummary, presets};

/// A saved fan-control profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// Schema version this document was written with.
    pub schema: u32,
    pub name: String,
    pub graph: Graph,
}

impl Profile {
    pub fn new(name: impl Into<String>, graph: Graph) -> Self {
        Self {
            schema: CURRENT_SCHEMA,
            name: name.into(),
            graph,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("profile is malformed: {0}")]
    Malformed(#[from] serde_json::Error),

    #[error(
        "profile schema version {found} is newer than this build understands ({CURRENT_SCHEMA}); \
         update OpenFan rather than letting it guess"
    )]
    FromTheFuture { found: u32 },
}

/// Parse a profile, migrating it forward to the current schema.
pub fn load(json: &str) -> Result<Profile, ConfigError> {
    let profile: Profile = serde_json::from_str(json)?;
    migrate(profile)
}

/// Bring a profile up to [`CURRENT_SCHEMA`].
///
/// Refuses profiles from a *newer* schema outright. Loading one by ignoring the fields we
/// do not recognise would mean quietly discarding part of a user's configuration — and in
/// this application, a discarded field could be a temperature limit.
pub fn migrate(profile: Profile) -> Result<Profile, ConfigError> {
    if profile.schema > CURRENT_SCHEMA {
        return Err(ConfigError::FromTheFuture {
            found: profile.schema,
        });
    }
    // No historical versions to migrate from yet. Each future bump adds a step here.
    Ok(profile)
}

/// Importers for other tools' configuration formats.
///
/// Interoperability only: these parse documented-by-observation on-disk formats. See the
/// clean-room rules in `plans/open-fan.md` — nothing in this module may derive from
/// decompiled code.
pub mod import {
    // Phase 7.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_round_trips() {
        let profile = Profile::new("Quiet", Graph::default());
        let json = serde_json::to_string(&profile).unwrap();
        assert_eq!(load(&json).unwrap(), profile);
    }

    #[test]
    fn a_future_profile_is_refused_rather_than_partially_loaded() {
        let mut profile = Profile::new("Quiet", Graph::default());
        profile.schema = CURRENT_SCHEMA + 1;
        let json = serde_json::to_string(&profile).unwrap();
        assert!(matches!(
            load(&json),
            Err(ConfigError::FromTheFuture { .. })
        ));
    }
}
