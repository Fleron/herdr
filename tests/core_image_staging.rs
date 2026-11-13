#![cfg(unix)]

mod core_support;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use core_support::CoreSession;
use herdr_core_tui::image_staging::StagedImage;

#[test]
fn actual_core_stages_a_large_image_in_private_files_and_cleans_only_its_own_path() {
    let session = CoreSession::new();
    let client = session.client();
    let bytes: Vec<u8> = (0..9 * 1024 * 1024 + 17)
        .map(|index| (index % 251) as u8)
        .collect();
    let mut image = StagedImage::prepare(&client, None, "png").unwrap();
    let path = image.path().to_owned();
    let directory = Path::new(&path).parent().unwrap();
    assert!(!directory.exists());
    image
        .upload(&client, &bytes, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let unrelated = session.root.join("unrelated.png");
    std::fs::write(&unrelated, b"keep this file").unwrap();
    image.remove(&client).unwrap();
    image.remove(&client).unwrap();
    assert!(!directory.exists());
    assert_eq!(std::fs::read(unrelated).unwrap(), b"keep this file");
    session.finish();
}

#[test]
fn staging_refuses_another_core_and_preserves_an_unowned_directory() {
    let first = CoreSession::new();
    let second = CoreSession::new();
    let client = first.client();
    let mut image = StagedImage::prepare(&client, None, "png").unwrap();
    let path = image.path().to_owned();
    let directory = Path::new(&path).parent().unwrap();
    assert!(image
        .upload(&second.client(), b"image", &AtomicBool::new(false))
        .is_err());
    assert!(!directory.exists());
    std::fs::create_dir(directory).unwrap();
    std::fs::write(&path, b"existing user file").unwrap();
    assert!(image.remove(&client).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"existing user file");
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(directory).unwrap();
    first.finish();
    second.finish();
}
