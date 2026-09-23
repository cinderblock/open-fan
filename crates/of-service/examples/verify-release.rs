//! Check a published release exactly as an installed service would, before publishing it.
//!
//! The release workflow creates a **draft**, and GitHub does not serve assets of a draft,
//! so nothing updates until a human publishes it. This is what that human should run
//! first: it applies the same four gates the service applies, against the real artifacts,
//! using the same code and the same compiled-in key.
//!
//! ```text
//! gh release download <tag> --dir <dir>
//! cargo run -p of-service --example verify-release -- <dir> [version-to-pretend-we-are]
//! ```
//!
//! A release that fails here would fail on every machine that tried to install it — and
//! would fail *after* download, as a signature error, which reads like an attack rather
//! than like a mistake in the pipeline.

use std::path::PathBuf;

use of_service::update::{PUBLIC_KEY, Release, decide, keys};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().unwrap_or_else(|| ".relcheck".to_owned()));
    // What an existing installation would believe it is running. Anything older than the
    // release under test; the default is the version before any release existed.
    let pretend = args.next().unwrap_or_else(|| "0.0.0-a".to_owned());

    let manifest = std::fs::read_to_string(dir.join("latest.json"))?;
    let release: Release = serde_json::from_str(&manifest)?;
    println!("manifest version : {}", release.version);
    println!("manifest url     : {}", release.url);

    // 1. Would a service running `pretend` decide to install this at all?
    match decide::should_install(&pretend, &release) {
        Ok(version) => println!("[ok] a machine on {pretend} would install {version}"),
        Err(rejection) => {
            println!("[no] a machine on {pretend} would refuse it: {rejection}");
            println!("     (that is the right answer for a release that is not newer)");
        }
    }

    // 2. Does the signature parse, and does the key we ship parse?
    let key = keys::public_key(PUBLIC_KEY)?;
    let signature = keys::signature(&release.signature)?;
    println!("[ok] the shipped key and the release signature both parse");

    // 3. Does the signature actually cover these bytes?
    let installer = std::fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.to_string_lossy().ends_with("-setup.exe"))
        .ok_or_else(|| anyhow::anyhow!("no installer in {}", dir.display()))?;

    let bytes = std::fs::read(&installer)?;
    key.verify(&bytes, &signature, false)
        .map_err(|e| anyhow::anyhow!("signature does not verify: {e}"))?;
    println!(
        "[ok] {} verifies against the key compiled into this build",
        installer.file_name().unwrap_or_default().to_string_lossy()
    );

    // 4. Does the signature vouch for the version the manifest announces? This is what
    //    stops a tampered feed pairing a new version number with an older signed file.
    if !keys::version_matches(signature.trusted_comment(), &release.version) {
        anyhow::bail!(
            "the installer is signed but vouches for something other than {}: {:?}",
            release.version,
            signature.trusted_comment()
        );
    }
    println!("[ok] the signature vouches for version {}", release.version);

    println!("\nThis release is installable. Publishing the draft is what makes it visible.");
    Ok(())
}
