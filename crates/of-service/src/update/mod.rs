//! Updating OpenFan.
//!
//! Two paths, and the difference between them is *who runs the installer*:
//!
//! | Path | Who installs | Prompt | Chosen by |
//! | --- | --- | --- | --- |
//! | **Silent** | this service, as LocalSystem | none | the user opting in |
//! | **Prompted** | the window, via `ShellExecute` | one UAC dialogue | the default |
//!
//! Both install the same signed artifact. Only the prompt differs, which is exactly the
//! choice a user should get to make: some people want their machine to keep itself
//! current without being asked, and some want to be asked every time.
//!
//! # The silent path is the dangerous one, so it is the constrained one
//!
//! A LocalSystem service that downloads and executes code is the most powerful thing in
//! this product — considerably more so than one that spins fans. The rules:
//!
//! * **The service decides what it installs.** The endpoint and the verifying public key
//!   are compiled in. Nothing arriving over the pipe can supply a URL, a file, a version,
//!   a signature or a key. A caller may say "check now" and "install what you found", and
//!   that is the whole of its influence.
//! * **Signature before execution, always.** The installer is verified against the
//!   embedded minisign key before it is run, on both paths. A download that fails
//!   verification is deleted, not retried.
//! * **Never downwards.** Strictly-newer only, so a silent update cannot reinstate a
//!   version whose flaws are already known. See [`decide`].
//! * **Silent is opt-in.** Off until someone turns it on, and the setting lives in
//!   `%ProgramData%` where only an administrator can change it.
//!
//! # Handing the fans over during an update
//!
//! Nothing special is needed, which is the point of the service. The installer stops the
//! service before replacing files; stopping runs the dying breath and returns every
//! channel to the board's own fan curve; the new service starts and picks them up again.
//! Observed across a real upgrade, not assumed.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};

pub mod decide;
pub mod settings;

pub use decide::{Rejection, Release};
pub use settings::{UpdateSettings, load as load_settings, save as save_settings};

/// Where releases are described.
///
/// Compiled in and never taken from a request. Overridable at *build* time only, so a
/// fork or a test build can point elsewhere without that becoming a runtime input.
pub const FEED_URL: &str = match option_env!("OPENFAN_UPDATE_FEED") {
    Some(url) => url,
    None => "https://raw.githubusercontent.com/cinderblock/open-fan/master/releases/latest.json",
};

/// The minisign public key releases are signed with.
///
/// Empty until releases are actually signed, and an empty key means **verification
/// cannot succeed**, so neither update path will install anything. That is the correct
/// posture for an unsigned project: refuse, loudly, rather than install unverified code
/// as LocalSystem.
pub const PUBLIC_KEY: &str = match option_env!("OPENFAN_UPDATE_PUBKEY") {
    Some(key) => key,
    None => "",
};

const USER_AGENT: &str = concat!("OpenFan/", env!("CARGO_PKG_VERSION"));
const TIMEOUT: Duration = Duration::from_secs(30);

/// What the service knows about updates right now.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current_version: String,
    /// The version on offer, when one is newer than ours.
    pub available: Option<String>,
    pub notes: Option<String>,
    /// Why the last check found nothing installable, if it found something.
    pub rejected: Option<String>,
    /// Whether the service installs updates on its own.
    pub automatic: bool,
    /// True once releases are signed. While false neither path can install.
    pub verifiable: bool,
    /// Last failure, for the interface to show rather than swallow.
    pub error: Option<String>,
}

/// Ask the feed what the newest release is.
pub fn check(current: &str) -> anyhow::Result<(Option<Release>, Option<Rejection>)> {
    let body = ureq::get(FEED_URL)
        .header("User-Agent", USER_AGENT)
        .config()
        .timeout_global(Some(TIMEOUT))
        .build()
        .call()
        .context("fetching the update feed")?
        .body_mut()
        .read_to_string()
        .context("reading the update feed")?;

    let release: Release = serde_json::from_str(&body).context("parsing the update feed")?;

    match decide::should_install(current, &release) {
        Ok(_) => Ok((Some(release), None)),
        Err(rejection) => Ok((None, Some(rejection))),
    }
}

/// Download the installer and verify it against the embedded key.
///
/// Returns the path of a verified installer. A file that fails verification is removed
/// before returning, so nothing unverified is ever left on disk for something else to
/// find and run.
pub fn download_verified(release: &Release) -> anyhow::Result<PathBuf> {
    if PUBLIC_KEY.trim().is_empty() {
        bail!(
            "this build has no update signing key, so a downloaded installer cannot be \
             verified. Refusing to install unverified code."
        );
    }

    let mut bytes = Vec::new();
    {
        use std::io::Read as _;
        let mut response = ureq::get(&release.url)
            .header("User-Agent", USER_AGENT)
            .config()
            .timeout_global(Some(TIMEOUT))
            .build()
            .call()
            .context("downloading the installer")?;

        // A cap, because this runs as LocalSystem and an endless body is a trivially
        // cheap way to exhaust a machine's memory.
        const MAX_INSTALLER_BYTES: u64 = 256 * 1024 * 1024;
        response
            .body_mut()
            .as_reader()
            .take(MAX_INSTALLER_BYTES)
            .read_to_end(&mut bytes)
            .context("reading the installer")?;
    }

    let key = minisign_verify::PublicKey::decode(PUBLIC_KEY.trim())
        .map_err(|e| anyhow!("the embedded update key is unusable: {e}"))?;
    let signature = minisign_verify::Signature::decode(release.signature.trim())
        .map_err(|e| anyhow!("the release signature is malformed: {e}"))?;

    key.verify(&bytes, &signature, false)
        .map_err(|e| anyhow!("the downloaded installer failed signature verification: {e}"))?;

    let path = std::env::temp_dir().join(format!("OpenFan-{}-setup.exe", release.version));
    std::fs::write(&path, &bytes).context("writing the verified installer")?;
    Ok(path)
}

/// Run a verified installer silently, as this service.
///
/// The installer stops the service — which is us — so it is spawned detached and this
/// process is expected to be told to stop shortly afterwards. That is the same shutdown
/// path as any other stop: the dying breath runs and the fans go back to the board's
/// curve before the files are replaced.
#[cfg(windows)]
pub fn install_silently(installer: &std::path::Path) -> anyhow::Result<()> {
    use std::os::windows::process::CommandExt;

    // DETACHED_PROCESS: the installer must outlive us, because one of the first things it
    // does is stop this service.
    const DETACHED_PROCESS: u32 = 0x0000_0008;

    tracing::warn!(installer = %installer.display(), "applying a verified update silently");

    std::process::Command::new(installer)
        .arg("/S")
        .creation_flags(DETACHED_PROCESS)
        .spawn()
        .context("launching the verified installer")?;

    Ok(())
}

#[cfg(not(windows))]
pub fn install_silently(installer: &std::path::Path) -> anyhow::Result<()> {
    let _ = installer;
    bail!("silent installation is only implemented on Windows")
}

/// Whether this build can verify a release at all.
pub fn verifiable() -> bool {
    !PUBLIC_KEY.trim().is_empty()
}
