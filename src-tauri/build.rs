fn main() {
    // OpenFan embeds a manifest requesting administrator rights. PawnIO refuses a device
    // handle to an unprivileged process, so without elevation the app cannot read a single
    // temperature — it would fall back to the simulated backend and display a plausible,
    // entirely fictional machine. A UAC prompt is much the better outcome.
    let attributes = tauri_build::Attributes::new().windows_attributes(
        tauri_build::WindowsAttributes::new().app_manifest(include_str!("openfan.manifest")),
    );

    tauri_build::try_build(attributes).expect("failed to configure the Tauri build");
}
