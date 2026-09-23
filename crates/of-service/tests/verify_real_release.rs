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
///
/// A signature older than the installer beside it is **stale local artifacts**, not a bad
/// signature: rebuilding without re-signing leaves the two describing different bytes. It
/// is treated as "nothing signed to check" for the same reason this file exists at all —
/// a verification failure here reads as an attack, and sending somebody to look for one
/// over a rebuild they did themselves is the exact confusion these tests are meant to
/// prevent.
fn signed_pair() -> Option<(PathBuf, PathBuf)> {
    let dir = bundle();
    let entries = std::fs::read_dir(dir).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "sig") {
            let installer = path.with_extension("");
            if installer.is_file() {
                if is_stale(&installer, &path) {
                    eprintln!(
                        "{} is newer than its signature; rebuild and re-sign to check it",
                        installer.display()
                    );
                    return None;
                }
                return Some((installer, path));
            }
        }
    }
    None
}

/// Whether the installer has been rebuilt since it was signed.
fn is_stale(installer: &PathBuf, signature: &PathBuf) -> bool {
    let modified = |p: &PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (modified(installer), modified(signature)) {
        (Some(built), Some(signed)) => built > signed,
        // Without timestamps, assume the pair is good and let verification speak.
        _ => false,
    }
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

/// The manifest the release workflow writes, parsed and checked as the service would.
///
/// Generated locally by `.github/workflows/release.yml`'s own node snippet against real
/// artifacts. This is the join between two things that are easy to let drift: what CI
/// emits, and what the service can read.
#[test]
fn the_manifest_the_release_workflow_writes_is_one_the_service_accepts() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.latest-test.json");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        eprintln!("no generated manifest present; skipping");
        return;
    };

    let release: of_service::update::Release =
        serde_json::from_str(&text).expect("the workflow's manifest must deserialise");

    // Same gate a real check applies before a byte is downloaded.
    of_service::update::decide::should_install("0.0.0-alpha", &release)
        .expect("a newer release from our own manifest must be installable");

    // And the signature in it must be the one that vouches for the version it names.
    let signature = keys::signature(&release.signature).expect("manifest signature parses");
    assert!(
        keys::version_matches(signature.trusted_comment(), &release.version),
        "the manifest's signature does not vouch for its own version"
    );

    let Some((installer, _)) = signed_pair() else {
        return;
    };
    let bytes = std::fs::read(installer).expect("read installer");
    let key = keys::public_key(of_service::update::PUBLIC_KEY).expect("key");
    key.verify(&bytes, &signature, false)
        .expect("the manifest's signature must verify the installer it points at");
}
