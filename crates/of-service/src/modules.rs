//! Fetching the PawnIO hardware module.
//!
//! PawnIO installs a driver and no hardware modules; `LpcIO`, which is what reads a Super
//! I/O, is a separate download from the PawnIO.Modules releases. So a machine can have
//! PawnIO installed, working and running, and OpenFan still sees no hardware at all —
//! which is the state a fresh install lands in, and easily mistaken for "unsupported
//! board".
//!
//! # Why fetching is acceptable here
//!
//! The usual objection to downloading executable content does not apply, because
//! **PawnIO's signed edition verifies module signatures in the kernel**. A blob that has
//! been tampered with, truncated or substituted simply will not load. We are not being
//! asked to trust the transport; the driver checks the thing itself.
//!
//! Fetching from upstream also avoids redistributing an LGPL-2.1 binary that is not ours.
//!
//! # The constraints
//!
//! * **Version-pinned, with a hash we ship.** "Latest" is not a dependency. The release
//!   tag and the module's SHA-256 are compiled in and checked before the file is written.
//! * **User-initiated.** A fan controller that silently reaches out to the network on
//!   first run is surprising, and surprising people is how trust in software with kernel
//!   access is lost. Nothing here runs on its own.
//! * **Never on the control path.** This is called from a request, on a connection
//!   thread. A missing module leaves the engine on the simulated backend, which is a
//!   degraded state the service reports rather than an error it fails on.
//! * **Machine-wide.** It goes to `%ProgramData%`, because the engine runs as LocalSystem
//!   and cannot see a per-user directory.

use anyhow::{Context as _, bail};
use sha2::{Digest as _, Sha256};

/// The PawnIO.Modules release we have tested against.
pub const MODULE_RELEASE: &str = "0.2.11";

/// `LpcIO.bin` from that release.
///
/// Checked before the file is written. The kernel would reject a bad blob anyway, but
/// failing here names the problem — "what we downloaded is not what we expected" — rather
/// than leaving a mystery load failure later.
pub const LPCIO_SHA256: &str = "b3896a1cab0d808fca31fe2ebcae045d59dac690da87b17c858bb8da357eb45e";

const RELEASE_ZIP: &str = "https://github.com/namazso/PawnIO.Modules/releases/download";

/// Whether the module the Super I/O backend needs is already present.
pub fn lpcio_present() -> bool {
    of_hal_pawnio::module_search_dirs()
        .into_iter()
        .any(|dir| dir.join("LpcIO.bin").is_file())
}

/// Where a fetched module is written.
pub fn install_dir() -> Option<std::path::PathBuf> {
    of_hal_pawnio::machine_module_dir()
}

/// Download `LpcIO.bin` from the pinned upstream release and install it machine-wide.
///
/// Returns where it was written. Blocking; call it from a connection thread.
pub fn fetch_lpcio() -> anyhow::Result<std::path::PathBuf> {
    let dir = install_dir().context("no ProgramData directory to install a module into")?;
    let target = dir.join("LpcIO.bin");

    let url = format!(
        "{RELEASE_ZIP}/{MODULE_RELEASE}/release_{}.zip",
        MODULE_RELEASE.replace('.', "_")
    );
    tracing::info!(%url, "fetching the PawnIO hardware module");

    let mut archive = Vec::new();
    {
        use std::io::Read as _;
        // Bounded: this runs as LocalSystem and an endless body is a cheap way to
        // exhaust a machine's memory.
        const MAX: u64 = 32 * 1024 * 1024;
        ureq::get(&url)
            .header("User-Agent", concat!("OpenFan/", env!("CARGO_PKG_VERSION")))
            .config()
            .timeout_global(Some(std::time::Duration::from_secs(60)))
            .build()
            .call()
            .context("downloading the PawnIO module release")?
            .body_mut()
            .as_reader()
            .take(MAX)
            .read_to_end(&mut archive)
            .context("reading the PawnIO module release")?;
    }

    let blob = extract(&archive, "LpcIO.bin")?;

    let actual = hex(&Sha256::digest(&blob));
    if actual != LPCIO_SHA256 {
        bail!(
            "the downloaded LpcIO.bin does not match the hash this build expects \
             (got {actual}, wanted {LPCIO_SHA256}). Not installing it."
        );
    }

    std::fs::create_dir_all(&dir).context("creating the module directory")?;
    std::fs::write(&target, &blob).context("writing the module")?;
    tracing::info!(path = %target.display(), "installed the PawnIO hardware module");

    Ok(target)
}

/// Pull one file out of a zip archive.
///
/// Uses `enclosed_name`, which rejects entries whose path escapes the archive root. A zip
/// containing `..\..\windows\system32\something` is a real technique, and this code
/// runs as LocalSystem — not a place to hand-roll path handling.
fn extract(archive: &[u8], want: &str) -> anyhow::Result<Vec<u8>> {
    let mut cursor = std::io::Cursor::new(archive);
    let mut zip = zip::ZipArchive::new(&mut cursor).context("reading the release archive")?;

    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).context("reading an archive entry")?;
        // `enclosed_name` rejects paths that escape the archive root; a zip entry called
        // `..\\..\\windows\\system32\\something` is a real technique and this is not a
        // place to be clever about it.
        let name = entry
            .enclosed_name()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));

        if name.as_deref() == Some(want) {
            use std::io::Read as _;
            let mut blob = Vec::new();
            entry
                .read_to_end(&mut blob)
                .context("extracting the module")?;
            return Ok(blob);
        }
    }

    bail!("{want} was not in the release archive")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pinned_hash_is_a_sha256() {
        // A truncated or mistyped hash would still compare unequal and so would only ever
        // *reject* a good module — a confusing failure rather than a dangerous one, but
        // worth catching at build time.
        assert_eq!(LPCIO_SHA256.len(), 64);
        assert!(LPCIO_SHA256.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(LPCIO_SHA256, LPCIO_SHA256.to_ascii_lowercase());
    }

    #[test]
    fn the_release_is_pinned_rather_than_latest() {
        // "latest" is not a dependency. If this ever becomes a moving target, the hash
        // above stops meaning anything.
        assert!(
            MODULE_RELEASE
                .chars()
                .all(|c| c.is_ascii_digit() || c == '.')
        );
        assert!(!MODULE_RELEASE.contains("latest"));
    }

    #[test]
    fn hex_encodes_the_way_sha256sum_prints() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
    }

    #[test]
    fn an_archive_without_the_module_is_an_error_not_an_empty_blob() {
        // Silently installing nothing would leave discovery failing for a reason nobody
        // could find.
        assert!(extract(b"not a zip at all", "LpcIO.bin").is_err());
    }
}
