fn main() {
    // The manifest is still ours rather than Tauri's default, but now it asks for
    // *nothing*: the window runs unelevated and talks to the OpenFan service, which holds
    // the elevated hardware handle. It also declares Common Controls v6, which replacing
    // the default manifest silently drops — see the manifest's own comment.
    let attributes = tauri_build::Attributes::new().windows_attributes(
        tauri_build::WindowsAttributes::new().app_manifest(include_str!("openfan.manifest")),
    );

    tauri_build::try_build(attributes).expect("failed to configure the Tauri build");
}
