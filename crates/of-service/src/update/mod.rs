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
pub mod keys;
pub mod settings;

pub use decide::{Rejection, Release};
pub use settings::{UpdateSettings, load as load_settings, save as save_settings};

/// Where releases are described.
///
/// Compiled in and never taken from a request. Overridable at *build* time only, so a
/// fork or a test build can point elsewhere without that becoming a runtime input.
///
/// A release *asset* rather than a file in the repository, for two reasons. It always
/// resolves to the newest published release without anything having to be committed, and
/// — more usefully — GitHub does not serve assets of a **draft** release. So a release
/// can be built, signed and inspected while remaining invisible to every installation,
/// and publishing the draft is the moment it becomes an update. That is a deliberate
/// gate, not an accident of hosting.
pub const FEED_URL: &str = match option_env!("OPENFAN_UPDATE_FEED") {
    Some(url) => url,
    None => "https://github.com/cinderblock/open-fan/releases/latest/download/latest.json",
};

/// The minisign public key releases are signed with.
///
/// **Compiled in, and deliberately not configurable at runtime.** This is the single
/// thing standing between "the service installs an update" and "the service installs
/// whatever an attacker served", so it must not be reachable from a request, a
/// configuration file, or an environment variable read at start-up. A build-time override
/// exists only so a fork can sign with its own key.
///
/// The private half lives in the repository's `TAURI_SIGNING_PRIVATE_KEY` secret and
/// nowhere else that is checked in.
///
/// An empty key means verification **cannot** succeed, so neither update path installs
/// anything — the right posture for a build that has no key rather than a reason to skip
/// the check.
pub const PUBLIC_KEY: &str = match option_env!("OPENFAN_UPDATE_PUBKEY") {
    Some(key) => key,
    None => {
        "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEQxMTA0NjdFMkY5NTI3REIKUldUYko1VXZma1lRMGFERTZSelBMMVFYWGdrMktyK0k3WmVtVjBscWx2UDRNQVBhdFkySGZwS2wK"
    }
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
///
/// A **404 is not an error**. The feed is an asset of the latest published release, and
/// GitHub does not serve assets of a draft — so "nothing published yet" and "no release
/// is visible to you" both look like a missing file, and both mean the same thing to a
/// user: there is no update. Reporting that as a failure would put a red message in front
/// of everyone running the newest build.
pub fn check(current: &str) -> anyhow::Result<(Option<Release>, Option<Rejection>)> {
    let response = match ureq::get(FEED_URL)
        .header("User-Agent", USER_AGENT)
        .config()
        .timeout_global(Some(TIMEOUT))
        .build()
        .call()
    {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(404)) => {
            return Ok((None, Some(Rejection::NothingPublished)));
        }
        Err(e) => return Err(anyhow::Error::new(e).context("fetching the update feed")),
    };

    let body = response
        .into_body()
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

    // Both formats accepted: `tauri signer` base64-wraps minisign files, plain
    // `minisign` does not. See `keys`.
    let key = keys::public_key(PUBLIC_KEY)?;
    let signature = keys::signature(&release.signature)?;

    key.verify(&bytes, &signature, false)
        .map_err(|e| anyhow!("the downloaded installer failed signature verification: {e}"))?;

    // The signature proves the *bytes* are ours. It does not, on its own, prove they are
    // the version the feed claimed — and the feed is a plain JSON file whoever serves it
    // controls. Checking the signed trusted comment is what stops a tampered or replayed
    // manifest pairing a new version number with an older, genuinely signed installer.
    if !keys::version_matches(signature.trusted_comment(), &release.version) {
        bail!(
            "the installer is correctly signed but vouches for a different version than the \
             update feed announced ({}). Refusing it: this is what a tampered or replayed feed \
             looks like.",
            release.version
        );
    }

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
