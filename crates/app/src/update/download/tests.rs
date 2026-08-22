use crate::update::download::{expected_digest, flat_name, package_assets};
use crate::update::releases::Asset;

const DIGEST: &str = "9f2fcc7c586c1eba3c4b7b48d0f2a9a6e1c9c1a1b2c3d4e5f60718293a4b5c6d";

fn asset(name: &str, url: &str) -> Asset {
    Asset {
        name: name.to_owned(),
        url: url.to_owned(),
    }
}

fn published(name: &str) -> Vec<Asset> {
    vec![
        asset(
            name,
            &format!("https://github.com/f32y/NiumaTerm/releases/download/v1.3.0/{name}"),
        ),
        asset(
            &format!("{name}.sha256"),
            &format!("https://github.com/f32y/NiumaTerm/releases/download/v1.3.0/{name}.sha256"),
        ),
    ]
}

#[test]
fn the_package_is_taken_with_the_checksum_published_for_it() {
    let assets = published("NiumaTerm-windows-x86_64-v1.3.0.zip");
    let (package, checksum) = package_assets(&assets).expect("a package and its checksum");

    assert_eq!(package.name, "NiumaTerm-windows-x86_64-v1.3.0.zip");
    assert_eq!(checksum.name, "NiumaTerm-windows-x86_64-v1.3.0.zip.sha256");
}

#[test]
fn a_package_without_its_own_checksum_is_not_installable() {
    let mut assets = published("NiumaTerm-windows-x86_64-v1.3.0.zip");
    // A checksum for some other file is not one for this package.
    assets[1].name = "NiumaTerm-windows-x86_64-v1.2.0.zip.sha256".to_owned();

    assert!(package_assets(&assets).is_none());
    assert!(package_assets(&[]).is_none());
}

#[test]
fn a_package_hosted_somewhere_else_is_refused() {
    // The URL arrives in an API response, so a response naming another host is
    // the case this rejects; the release page it came from proves nothing about
    // where the bytes would come from.
    let assets = vec![
        asset(
            "NiumaTerm-windows-x86_64-v1.3.0.zip",
            "https://example.invalid/NiumaTerm-windows-x86_64-v1.3.0.zip",
        ),
        asset(
            "NiumaTerm-windows-x86_64-v1.3.0.zip.sha256",
            "https://example.invalid/NiumaTerm-windows-x86_64-v1.3.0.zip.sha256",
        ),
    ];

    assert!(package_assets(&assets).is_none());
}

#[test]
fn the_digest_is_read_out_of_a_sha256sum_line() {
    assert_eq!(
        expected_digest(&format!("{DIGEST}  NiumaTerm-windows-x86_64-v1.3.0.zip\n")).as_deref(),
        Some(DIGEST)
    );
    // GitHub serves the file as published, and a digest written in upper case
    // is the same digest.
    assert_eq!(
        expected_digest(&DIGEST.to_uppercase()).as_deref(),
        Some(DIGEST)
    );

    // Anything that is not a digest of the right length and alphabet cannot be
    // compared against one, and must not pass as a match either.
    assert_eq!(expected_digest(""), None);
    assert_eq!(expected_digest("not-a-digest  package.zip"), None);
    assert_eq!(expected_digest(&DIGEST[..63]), None);
    assert_eq!(expected_digest(&format!("{DIGEST}0")), None);
}

#[test]
fn only_flat_entry_names_are_unpacked() {
    assert_eq!(flat_name("NiumaTerm.exe"), Some("NiumaTerm.exe"));

    // Every form that would write outside the staging directory, plus the
    // directory entries a flat package does not contain.
    for rejected in [
        "",
        ".",
        "..",
        "../NiumaTerm.exe",
        "..\\NiumaTerm.exe",
        "nested/NiumaTerm.exe",
        "nested\\NiumaTerm.exe",
        "C:\\Windows\\System32\\NiumaTerm.exe",
        "/NiumaTerm.exe",
        "NiumaTerm.exe\0.txt",
    ] {
        assert_eq!(flat_name(rejected), None, "accepted `{rejected}`");
    }
}
