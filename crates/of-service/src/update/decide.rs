//! Deciding whether an update should be applied, without applying anything.
//!
//! Every rule that could let the wrong thing be installed lives here as a pure function
//! over described inputs, so it can be tested exhaustively. The half that touches the
//! network and the filesystem is deliberately dull by comparison.
//!
//! # The threat this is written against
//!
//! Silent updates are applied by a service running as **LocalSystem**. If a hostile party
//! can choose what that service installs, they own the machine — this is the single most
//! dangerous capability in the product, and considerably more dangerous than controlling
//! fans. So the rules are arranged around one principle: **the service decides what it
//! installs, and nothing that arrives over the pipe influences that choice.**
//!
//! Concretely, a caller can ask *"check now"* and *"install the update you found"*. It
//! cannot supply a URL, a file, a version, a signature, or a public key. The endpoint and
//! the verifying key are compiled in.

use semver::Version;

/// A release as the update feed describes it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Release {
    pub version: String,
    /// Where the installer lives. Must be HTTPS — see [`Rejection::InsecureUrl`].
    pub url: String,
    /// Detached minisign signature of the installer.
    pub signature: String,
    #[serde(default)]
    pub notes: Option<String>,
}

/// Why an available release will not be installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// The feed offered something we already have, or older.
    NotNewer { current: String, offered: String },
    /// A version string we cannot compare. Refusing beats guessing at ordering.
    Unparseable { version: String },
    /// The download is not over HTTPS. A signature check makes tampering detectable, but
    /// plaintext also leaks which machines run which version, and there is no reason to
    /// accept it.
    InsecureUrl { url: String },
    /// No signature offered. Never install unsigned code as LocalSystem.
    Unsigned,
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotNewer { current, offered } => {
                write!(f, "already on {current}; the feed offers {offered}")
            }
            Self::Unparseable { version } => {
                write!(f, "cannot compare version {version:?} against ours")
            }
            Self::InsecureUrl { url } => write!(f, "refusing a download that is not HTTPS: {url}"),
            Self::Unsigned => write!(f, "the release carries no signature"),
        }
    }
}

/// Whether a release should be downloaded at all.
///
/// Called before a single byte is fetched, so a malformed or downgrade release costs
/// nothing. The signature is verified separately, after download and before execution.
pub fn should_install(current: &str, release: &Release) -> Result<Version, Rejection> {
    if release.signature.trim().is_empty() {
        return Err(Rejection::Unsigned);
    }

    if !release.url.starts_with("https://") {
        return Err(Rejection::InsecureUrl {
            url: release.url.clone(),
        });
    }

    let offered = parse(&release.version).ok_or_else(|| Rejection::Unparseable {
        version: release.version.clone(),
    })?;
    let running = parse(current).ok_or_else(|| Rejection::Unparseable {
        version: current.to_owned(),
    })?;

    // Strictly newer. Equal is not an update, and older is a downgrade — which, applied
    // silently by a LocalSystem service, is how a known-vulnerable version gets put back
    // on a machine that had already moved past it.
    if offered > running {
        Ok(offered)
    } else {
        Err(Rejection::NotNewer {
            current: current.to_owned(),
            offered: release.version.clone(),
        })
    }
}

/// Parse a version, tolerating a leading `v`.
fn parse(text: &str) -> Option<Version> {
    Version::parse(text.trim().trim_start_matches('v')).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(version: &str) -> Release {
        Release {
            version: version.to_owned(),
            url: "https://example.invalid/OpenFan-setup.exe".to_owned(),
            signature: "untrusted comment: signature\nRWQ...".to_owned(),
            notes: None,
        }
    }

    #[test]
    fn a_newer_release_is_accepted() {
        assert_eq!(
            should_install("0.1.0", &release("0.2.0")),
            Ok(Version::new(0, 2, 0))
        );
        assert!(should_install("0.1.0", &release("v0.1.1")).is_ok());
    }

    #[test]
    fn the_same_version_is_not_an_update() {
        assert_eq!(
            should_install("1.2.3", &release("1.2.3")),
            Err(Rejection::NotNewer {
                current: "1.2.3".into(),
                offered: "1.2.3".into(),
            })
        );
    }

    #[test]
    fn a_downgrade_is_refused() {
        // The dangerous one. A LocalSystem service that silently accepts an older build
        // is a way to reinstate a version whose vulnerability is already known.
        assert!(matches!(
            should_install("2.0.0", &release("1.9.9")),
            Err(Rejection::NotNewer { .. })
        ));
        assert!(matches!(
            should_install("1.0.0", &release("0.0.1")),
            Err(Rejection::NotNewer { .. })
        ));
    }

    #[test]
    fn an_unsigned_release_is_refused_before_anything_else() {
        // Checked first: there is no version comparison worth doing on something we would
        // never execute.
        let mut unsigned = release("99.0.0");
        unsigned.signature = String::new();
        assert_eq!(should_install("0.1.0", &unsigned), Err(Rejection::Unsigned));

        unsigned.signature = "   \n ".to_owned();
        assert_eq!(should_install("0.1.0", &unsigned), Err(Rejection::Unsigned));
    }

    #[test]
    fn a_plaintext_download_is_refused() {
        let mut insecure = release("99.0.0");
        insecure.url = "http://example.invalid/setup.exe".to_owned();
        assert!(matches!(
            should_install("0.1.0", &insecure),
            Err(Rejection::InsecureUrl { .. })
        ));

        // And not by a prefix trick.
        insecure.url = "https-evil://example.invalid/setup.exe".to_owned();
        assert!(matches!(
            should_install("0.1.0", &insecure),
            Err(Rejection::InsecureUrl { .. })
        ));
    }

    #[test]
    fn an_uncomparable_version_is_refused_rather_than_guessed_at() {
        // "latest" sorts after nothing. Installing on a string we cannot order is how a
        // downgrade slips through.
        assert!(matches!(
            should_install("0.1.0", &release("latest")),
            Err(Rejection::Unparseable { .. })
        ));
        assert!(matches!(
            should_install("0.1.0", &release("")),
            Err(Rejection::Unparseable { .. })
        ));
    }

    #[test]
    fn prerelease_ordering_follows_semver_rather_than_string_order() {
        // 0.2.0-rc.1 is *older* than 0.2.0, though it sorts after it as text.
        assert!(should_install("0.2.0", &release("0.2.0-rc.1")).is_err());
        assert!(should_install("0.2.0-rc.1", &release("0.2.0")).is_ok());
    }

    #[test]
    fn every_rejection_explains_itself() {
        // These reach a user through the interface, so "update failed" is not enough.
        for rejection in [
            Rejection::NotNewer {
                current: "1.0.0".into(),
                offered: "0.9.0".into(),
            },
            Rejection::Unparseable {
                version: "latest".into(),
            },
            Rejection::InsecureUrl {
                url: "http://x".into(),
            },
            Rejection::Unsigned,
        ] {
            let text = rejection.to_string();
            assert!(text.len() > 15, "{rejection:?} -> {text:?}");
        }
    }
}
