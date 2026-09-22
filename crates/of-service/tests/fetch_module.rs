//! Fetch the real PawnIO module from the real upstream release.
//!
//! Network-dependent, so it is `#[ignore]`d and run deliberately:
//!
//! ```sh
//! cargo test -p of-service --test fetch_module -- --ignored --nocapture
//! ```
//!
//! It exists because every part of this is a guess until it runs once: the release URL
//! shape, the archive layout, the file name inside it, and the pinned hash. Any of them
//! being wrong leaves a fresh machine unable to see its own hardware, which is the least
//! debuggable failure this product has.

#[test]
#[ignore = "downloads from the PawnIO.Modules release"]
fn the_pinned_module_downloads_and_matches_its_hash() {
    let path = of_service::modules::fetch_lpcio().expect("the pinned module must fetch");

    assert!(path.is_file(), "nothing was written to {}", path.display());
    let bytes = std::fs::read(&path).expect("read the installed module");
    assert!(!bytes.is_empty(), "the installed module is empty");

    println!("installed {} bytes to {}", bytes.len(), path.display());

    // And discovery must now find it, which is the thing that actually matters.
    assert!(
        of_service::modules::lpcio_present(),
        "the module was written but discovery still cannot see it"
    );
}
