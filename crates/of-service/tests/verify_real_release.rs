//! Verify a real signed installer exactly as the service would.
//!
//! The unit tests cover the *formats* — key parsing, version binding — against fixtures.
//! This closes the loop on an actual artifact: the installer this repository builds,
//! signed with the key CI uses, checked by the code that decides whether to run it.
//!
//! It is the test that would have caught a mismatch between what `tauri signer` writes
//! and what `minisign-verify` reads, which is the kind of thing that does not fail loudly
//! — it fails as "signature verification failed" on a perfectly good release and sends
//! you looking for an attacker.
//!
//! Skipped when there is nothing built to check, so it is free on a clean checkout and
//! meaningful straight after `bun run tauri build` and a signing step.

use std::path::PathBuf;

use of_service::update::keys;

fn bundle() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/release/bundle/nsis")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("does-not-exist"))
}

/// The installer and its detached signature, if a signed build is present.
fn signed_pair() -> Option<(PathBuf, PathBuf)> {
    let dir = bundle();
    let entries = std::fs::read_dir(dir).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "sig") {
            let installer = path.with_extension("");
            if installer.is_file() {
                return Some((installer, path));
            }
        }
    }
    None
}

#[test]
fn a_real_signed_installer_verifies_against_the_shipped_key() {
    let Some((installer, signature_path)) = signed_pair() else {
        eprintln!("no signed installer present; skipping");
        return;
    };

    let bytes = std::fs::read(&installer).expect("read installer");
    let signature_text = std::fs::read_to_string(&signature_path).expect("read signature");

    let key = keys::public_key(of_service::update::PUBLIC_KEY)
        .expect("the key compiled into this build must parse");
    let signature = keys::signature(&signature_text).expect("the release signature must parse");

    key.verify(&bytes, &signature, false)
        .expect("the installer this repository built must verify against the key it ships");
}

#[test]
fn the_signature_vouches_for_the_version_the_manifest_declares() {
    let Some((_, signature_path)) = signed_pair() else {
        eprintln!("no signed installer present; skipping");
        return;
    };

    let signature_text = std::fs::read_to_string(&signature_path).expect("read signature");
    let signature = keys::signature(&signature_text).expect("parse signature");

    // The version in tauri.conf.json is what a release announces, and what the signature
    // must independently vouch for. A mismatch here is the tampered-feed case.
    let manifest = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/tauri.conf.json"),
    )
    .expect("read tauri.conf.json");
    let version = manifest
        .lines()
        .find_map(|line| line.trim().strip_prefix("\"version\": \""))
        .and_then(|rest| rest.split('"').next())
        .expect("a version in tauri.conf.json");

    assert!(
        keys::version_matches(signature.trusted_comment(), version),
        "signature comment {:?} does not vouch for version {version}",
        signature.trusted_comment()
    );
}
