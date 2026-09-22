//! Reading the key and signature formats our release pipeline produces.
//!
//! `tauri signer` emits **base64-wrapped minisign files**, not bare minisign ones: the
//! public key is base64 of a two-line `.pub` file, and a signature is base64 of a
//! four-line `.minisig`. `minisign-verify` wants the unwrapped text. Getting this wrong
//! does not fail loudly — it fails as "signature verification failed" on a perfectly good
//! release, which is indistinguishable from an attack and would send someone hunting in
//! entirely the wrong place.
//!
//! So both forms are accepted, and which one arrived is decided by *trying* rather than by
//! guessing from shape. That also means a release signed with plain `minisign` instead of
//! the Tauri CLI still verifies.

use anyhow::{Context as _, anyhow};
use base64::Engine as _;
use minisign_verify::{PublicKey, Signature};

/// Unwrap one layer of base64, if that is what this is.
///
/// Returns `None` when the text is not base64 or does not decode to something text-like,
/// which is the ordinary case for an already-unwrapped minisign file.
fn unwrap_base64(text: &str) -> Option<String> {
    let compact: String = text.split_whitespace().collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(compact)
        .ok()?;
    let decoded = String::from_utf8(bytes).ok()?;
    // A minisign file always starts with a comment line. Anything else means we decoded
    // base64 that was not a wrapper, and we should use the original.
    decoded.starts_with("untrusted comment:").then_some(decoded)
}

/// Parse a public key in either the bare or the base64-wrapped form.
pub fn public_key(text: &str) -> anyhow::Result<PublicKey> {
    let text = text.trim();
    if let Some(unwrapped) = unwrap_base64(text) {
        return PublicKey::decode(unwrapped.trim())
            .map_err(|e| anyhow!("the embedded update key is unusable: {e}"));
    }

    PublicKey::decode(text)
        .or_else(|_| PublicKey::from_base64(text))
        .map_err(|e| anyhow!("the embedded update key is unusable: {e}"))
        .context("public key was neither a minisign file nor a bare key")
}

/// Parse a signature in either the bare or the base64-wrapped form.
pub fn signature(text: &str) -> anyhow::Result<Signature> {
    let text = text.trim();
    let candidate = unwrap_base64(text).unwrap_or_else(|| text.to_owned());

    Signature::decode(candidate.trim())
        .map_err(|e| anyhow!("the release signature is malformed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real public key for this project's releases, as `tauri signer` wrote it.
    const WRAPPED: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEQxMTA0NjdFMkY5NTI3REIKUldUYko1VXZma1lRMGFERTZSelBMMVFYWGdrMktyK0k3WmVtVjBscWx2UDRNQVBhdFkySGZwS2wK";

    const BARE: &str = "untrusted comment: minisign public key: D110467E2F9527DB\n\
                        RWTbJ5UvfkYQ0aDE6RzPL1QXXgk2Kr+I7ZemV0lqlvP4MAPatY2HfpKl";

    #[test]
    fn the_key_our_release_pipeline_produces_parses() {
        // The exact string `tauri signer generate` emitted for this project. If this ever
        // stops parsing, every release stops being installable.
        public_key(WRAPPED).expect("the wrapped release key must parse");
    }

    #[test]
    fn a_bare_minisign_key_parses_too() {
        // So a release signed with plain `minisign` rather than the Tauri CLI still works.
        public_key(BARE).expect("a bare minisign key must parse");
    }

    #[test]
    fn both_forms_yield_the_same_key() {
        // The wrapper is an encoding, not a different key. If these disagreed, half our
        // tooling would be verifying against something else.
        assert_eq!(
            public_key(WRAPPED).expect("wrapped"),
            public_key(BARE).expect("bare"),
        );
    }

    #[test]
    fn surrounding_whitespace_does_not_break_it() {
        // Keys travel through environment variables, YAML and shells, all of which add
        // newlines. A stray one must not cost a release.
        public_key(&format!("  \n{WRAPPED}\n  ")).expect("padded wrapped");
        public_key(&format!("\n{BARE}\n")).expect("padded bare");
    }

    #[test]
    fn rubbish_is_refused_rather_than_accepted_as_some_key() {
        for bad in ["", "   ", "not a key", "AAAA"] {
            assert!(public_key(bad).is_err(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn a_malformed_signature_is_refused() {
        for bad in ["", "untrusted comment: only one line", "garbage"] {
            assert!(signature(bad).is_err(), "{bad:?} must not parse");
        }
    }
}

/// Whether a signature's trusted comment vouches for this exact version.
///
/// Closes a hole that signature verification alone does not: the feed is a plain JSON
/// file, so whoever serves it chooses the version string. Without this, an attacker who
/// can tamper with the feed — or merely replay an old one — can announce `9.9.9` while
/// serving a genuinely signed *older* installer. The signature checks out, the version
/// comparison passes, and the machine is quietly rolled back to a build whose flaws are
/// already known.
///
/// `tauri signer sign --app-version` writes the version into the **trusted** comment,
/// which is covered by the signature and therefore cannot be edited without invalidating
/// it. So the manifest's claim is checked against the signed one.
///
/// A signature with no version in its comment is rejected rather than waved through:
/// treating "no claim" as "any claim" would make the check trivially bypassable by
/// stripping it.
pub fn version_matches(trusted_comment: &str, expected: &str) -> bool {
    let wanted = expected.trim().trim_start_matches('v');

    trusted_comment
        .split_whitespace()
        .filter_map(|field| field.strip_prefix("version:"))
        .any(|found| found.trim().trim_start_matches('v') == wanted)
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn a_comment_vouching_for_this_version_matches() {
        assert!(version_matches(
            "timestamp:1758000000\tfile:OpenFan_0.2.0_x64-setup.exe\tversion:0.2.0",
            "0.2.0"
        ));
        // Either side may carry a leading v.
        assert!(version_matches("version:v1.2.3", "1.2.3"));
        assert!(version_matches("version:1.2.3", "v1.2.3"));
    }

    #[test]
    fn a_comment_vouching_for_a_different_version_is_refused() {
        // The attack: a tampered feed announcing 9.9.9 while serving a validly signed
        // older installer. The signature verifies; this is what catches it.
        assert!(!version_matches(
            "timestamp:1 file:OpenFan_0.1.0_x64-setup.exe version:0.1.0",
            "9.9.9"
        ));
    }

    #[test]
    fn a_comment_with_no_version_is_refused_rather_than_trusted() {
        // "No claim" must not mean "any claim", or the check is bypassed by removing it.
        assert!(!version_matches(
            "timestamp:1758000000 file:setup.exe",
            "0.2.0"
        ));
        assert!(!version_matches("", "0.2.0"));
    }

    #[test]
    fn a_version_that_merely_contains_the_expected_one_does_not_match() {
        // 0.2.0 must not be satisfied by 0.2.01 or 10.2.0.
        assert!(!version_matches("version:0.2.01", "0.2.0"));
        assert!(!version_matches("version:10.2.0", "0.2.0"));
    }

    #[test]
    fn the_filename_is_not_mistaken_for_the_version() {
        // The file field routinely contains the version as a substring. Only the
        // `version:` field counts.
        assert!(!version_matches(
            "file:OpenFan_9.9.9_x64-setup.exe",
            "9.9.9"
        ));
    }
}
