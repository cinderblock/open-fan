//! The service's persisted settings.
//!
//! Kept in `%ProgramData%\OpenFan\settings.json`, which is machine-wide and writable only
//! by administrators. That matters for one setting in particular: whether this service
//! installs updates on its own, without a prompt. A per-user file would let any user turn
//! on unattended LocalSystem installs for the whole machine.
//!
//! A missing or unreadable file yields the defaults rather than an error. The service must
//! start and control fans whatever state its configuration is in; refusing to run because
//! a preferences file is malformed would be a spectacular own goal.

use std::path::PathBuf;

/// How the service handles updates.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdateSettings {
    /// Install verified updates without asking.
    ///
    /// **Off by default, deliberately.** Unattended installation by a LocalSystem service
    /// is the most powerful thing this product can do, and it should be something a person
    /// chose rather than something they received.
    pub automatic: bool,
    /// How often to look, in hours, when automatic is on.
    pub check_interval_hours: u32,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            automatic: false,
            check_interval_hours: 24,
        }
    }
}

impl UpdateSettings {
    /// The checking interval, clamped to something sane.
    ///
    /// A hostile or fat-fingered `0` would otherwise mean "hammer the update feed in a
    /// tight loop", which is a denial-of-service against our own infrastructure and a
    /// conspicuous waste of the user's network.
    pub fn interval(&self) -> std::time::Duration {
        const MIN_HOURS: u32 = 1;
        const MAX_HOURS: u32 = 24 * 30;
        let hours = self.check_interval_hours.clamp(MIN_HOURS, MAX_HOURS);
        std::time::Duration::from_secs(u64::from(hours) * 3600)
    }
}

/// Where settings live: machine-wide, administrator-writable.
pub fn path() -> Option<PathBuf> {
    std::env::var_os("ProgramData")
        .map(|base| PathBuf::from(base).join("OpenFan").join("settings.json"))
}

/// Read the settings, falling back to defaults.
///
/// Never fails. A corrupt file is reported and ignored, because the alternative — a
/// service that will not start — is far worse than one running on defaults.
pub fn load() -> UpdateSettings {
    let Some(path) = path() else {
        return UpdateSettings::default();
    };

    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(settings) => settings,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "settings unreadable; using defaults");
                UpdateSettings::default()
            }
        },
        // Absent is the ordinary first-run case, not a problem worth logging loudly.
        Err(_) => UpdateSettings::default(),
    }
}

/// Persist the settings.
pub fn save(settings: &UpdateSettings) -> anyhow::Result<()> {
    let path = path().ok_or_else(|| anyhow::anyhow!("no ProgramData directory"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(settings)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_updates_are_off_until_someone_turns_them_on() {
        // The default a machine gets if nobody ever opens the settings. Unattended
        // installation by a LocalSystem service should be chosen, not received.
        assert!(!UpdateSettings::default().automatic);
    }

    #[test]
    fn a_zero_interval_cannot_become_a_tight_loop() {
        let settings = UpdateSettings {
            automatic: true,
            check_interval_hours: 0,
        };
        assert!(settings.interval() >= std::time::Duration::from_secs(3600));
    }

    #[test]
    fn an_absurd_interval_is_clamped_rather_than_overflowing() {
        let settings = UpdateSettings {
            automatic: true,
            check_interval_hours: u32::MAX,
        };
        // Finite, and no multiplication overflow on the way.
        assert!(settings.interval() <= std::time::Duration::from_secs(24 * 3600 * 30));
    }

    #[test]
    fn settings_round_trip_through_json() {
        let settings = UpdateSettings {
            automatic: true,
            check_interval_hours: 6,
        };
        let text = serde_json::to_string(&settings).expect("encode");
        let back: UpdateSettings = serde_json::from_str(&text).expect("decode");
        assert_eq!(settings, back);
    }

    #[test]
    fn a_partial_file_fills_in_defaults_rather_than_failing() {
        // Forward compatibility: an older service reading a newer file, or a
        // hand-edited one, must still start.
        let back: UpdateSettings = serde_json::from_str("{}").expect("decode");
        assert_eq!(back, UpdateSettings::default());

        let back: UpdateSettings = serde_json::from_str(r#"{"automatic":true}"#).expect("decode");
        assert!(back.automatic);
        assert_eq!(back.check_interval_hours, 24);
    }
}
