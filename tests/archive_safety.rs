//! Validation of damaged archives, and extraction of deliberately hostile ones.
//!
//! These archives are assembled byte by byte rather than through the writer, so
//! the reader is tested against input it would never produce itself.

use std::fs;
use std::path::{Path, PathBuf};

use flatbak::backup::container::{self, Header, Trailer};
use flatbak::backup::reader::{self, Backup, ConflictChoice, RestoreRequest, RestoreSelection};
use flatbak::error::{ArchiveError, Issue};
use flatbak::flatpak::Flatpak;
use flatbak::util::Cancel;

const APP_ID: &str = "org.example.Demo";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "flatbak-safety-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------- tar building

/// Writes a name straight into the header, bypassing any validation the tar
/// crate would apply, so hostile paths can be produced at all.
fn set_name(header: &mut tar::Header, name: &str) {
    let bytes = name.as_bytes();
    assert!(bytes.len() < 100, "test names must fit the old tar header");
    let old = header.as_old_mut();
    old.name[..bytes.len()].copy_from_slice(bytes);
}

fn set_link(header: &mut tar::Header, target: &str) {
    let bytes = target.as_bytes();
    assert!(bytes.len() < 100);
    let old = header.as_old_mut();
    old.linkname[..bytes.len()].copy_from_slice(bytes);
}

enum Entry {
    File { name: String, data: Vec<u8> },
    Dir { name: String },
    Symlink { name: String, target: String },
    HardLink { name: String, target: String },
}

fn file(name: &str, data: &str) -> Entry {
    Entry::File {
        name: name.to_owned(),
        data: data.as_bytes().to_vec(),
    }
}

/// Assembles a tar stream from raw entries, including the trailing zero blocks.
fn build_tar(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::new();
    for entry in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);

        let payload: &[u8] = match entry {
            Entry::File { name, data } => {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(data.len() as u64);
                set_name(&mut header, name);
                data
            }
            Entry::Dir { name } => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_mode(0o755);
                header.set_size(0);
                set_name(&mut header, name);
                &[]
            }
            Entry::Symlink { name, target } => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                set_name(&mut header, name);
                set_link(&mut header, target);
                &[]
            }
            Entry::HardLink { name, target } => {
                header.set_entry_type(tar::EntryType::Link);
                header.set_size(0);
                set_name(&mut header, name);
                set_link(&mut header, target);
                &[]
            }
        };

        header.set_cksum();
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(payload);
        // Pad the data to the 512-byte block boundary.
        let remainder = payload.len() % 512;
        if remainder != 0 {
            out.extend(std::iter::repeat(0u8).take(512 - remainder));
        }
    }
    // End-of-archive marker.
    out.extend(std::iter::repeat(0u8).take(1024));
    out
}

// ---------------------------------------------------- container assembly

fn manifest_toml(format_version: u32, data_included: bool) -> String {
    format!(
        r#"[flatbak]
format_version = {format_version}
created_at = "2026-09-08T12:00:00Z"
created_by = "test"
host_arch = "x86_64"
app_count = 1

[[app]]
id = "{APP_ID}"
name = "Demo"
branch = "stable"
arch = "x86_64"
origin = "flathub"
installation = "user"
ref = "app/{APP_ID}/x86_64/stable"
data_included = {data_included}
data_bytes = 64
data_path = "data/{APP_ID}"
"#
    )
}

/// Writes a complete, internally consistent `.flatbak` file.
fn write_archive(path: &Path, manifest: &str, payload_tar: &[u8]) {
    let manifest_z = zstd::encode_all(manifest.as_bytes(), 1).unwrap();
    let payload_z = zstd::encode_all(payload_tar, 1).unwrap();

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&Header::new(manifest_z.len() as u32, payload_z.len() as u64).encode());
    bytes.extend_from_slice(&manifest_z);
    bytes.extend_from_slice(&payload_z);

    let digest: [u8; 32] = {
        use sha2::Digest;
        sha2::Sha256::digest(&payload_z).into()
    };
    bytes.extend_from_slice(&Trailer { payload_sha256: digest }.encode());

    fs::write(path, bytes).unwrap();
}

fn archive_error(error: &anyhow::Error) -> &ArchiveError {
    error
        .downcast_ref::<ArchiveError>()
        .unwrap_or_else(|| panic!("expected an ArchiveError, got: {error:#}"))
}

// ------------------------------------------------------------------- tests

#[test]
fn rejects_files_that_are_not_archives() {
    let root = scratch("notanarchive");

    let empty = root.join("empty.flatbak");
    fs::write(&empty, b"").unwrap();
    assert!(matches!(
        archive_error(&Backup::open(&empty).unwrap_err()),
        ArchiveError::NotAFlatbakArchive(_)
    ));

    let wrong = root.join("wrong.flatbak");
    fs::write(&wrong, vec![b'X'; 4096]).unwrap();
    assert!(matches!(
        archive_error(&Backup::open(&wrong).unwrap_err()),
        ArchiveError::NotAFlatbakArchive(_)
    ));

    let short = root.join("short.flatbak");
    fs::write(&short, b"FLATBAK\x1a\x01").unwrap();
    assert!(matches!(
        archive_error(&Backup::open(&short).unwrap_err()),
        ArchiveError::Truncated
    ));

    fs::remove_dir_all(&root).ok();
}

#[test]
fn detects_truncation_and_trailing_junk() {
    let root = scratch("truncation");
    let path = root.join("a.flatbak");
    write_archive(&path, &manifest_toml(1, true), &build_tar(&[file(
        &format!("data/{APP_ID}/config/x"),
        "hello",
    )]));

    let good = fs::read(&path).unwrap();

    // Losing the tail costs us the trailer.
    let cut = root.join("cut.flatbak");
    fs::write(&cut, &good[..good.len() - 8]).unwrap();
    assert!(matches!(
        archive_error(&Backup::open(&cut).unwrap_err()),
        ArchiveError::Truncated
    ));

    // Extra bytes mean the file no longer matches what the header describes.
    let extra = root.join("extra.flatbak");
    let mut padded = good.clone();
    padded.extend_from_slice(b"junk");
    fs::write(&extra, &padded).unwrap();
    assert!(matches!(
        archive_error(&Backup::open(&extra).unwrap_err()),
        ArchiveError::Corrupt(_)
    ));

    // A flipped bit in the header is caught by its CRC.
    let bent = root.join("bent.flatbak");
    let mut bytes = good.clone();
    bytes[16] ^= 0x08;
    fs::write(&bent, &bytes).unwrap();
    assert!(matches!(
        archive_error(&Backup::open(&bent).unwrap_err()),
        ArchiveError::Corrupt(_) | ArchiveError::Truncated
    ));

    fs::remove_dir_all(&root).ok();
}

#[test]
fn detects_a_tampered_payload_before_restoring() {
    let root = scratch("tampered");
    let path = root.join("t.flatbak");
    write_archive(
        &path,
        &manifest_toml(1, true),
        &build_tar(&[file(&format!("data/{APP_ID}/config/x"), "hello")]),
    );

    let mut bytes = fs::read(&path).unwrap();
    let payload_start = container::HEADER_LEN as usize
        + Backup::open(&path).unwrap().header.manifest_len as usize;
    // Flip a bit inside the compressed payload, leaving header and lengths intact.
    bytes[payload_start + 4] ^= 0x40;
    fs::write(&path, &bytes).unwrap();

    // The framing still adds up, so opening succeeds \u{2014} that is the point of
    // verifying separately, before anything is written to disk.
    let backup = Backup::open(&path).expect("framing is still consistent");
    let error = backup
        .verify_payload(&Cancel::new(), &mut |_, _| {})
        .unwrap_err();
    assert!(matches!(
        archive_error(&error),
        ArchiveError::ChecksumMismatch
    ));

    fs::remove_dir_all(&root).ok();
}

#[test]
fn refuses_manifests_from_a_newer_format() {
    let root = scratch("future");
    let path = root.join("future.flatbak");
    write_archive(&path, &manifest_toml(99, false), &build_tar(&[]));

    match archive_error(&Backup::open(&path).unwrap_err()) {
        ArchiveError::UnsupportedVersion { found, supported } => {
            assert_eq!(*found, 99);
            assert_eq!(*supported, flatbak::config::MANIFEST_FORMAT_VERSION_MAX);
        }
        other => panic!("expected UnsupportedVersion, got {other:?}"),
    }

    fs::remove_dir_all(&root).ok();
}

#[test]
fn extraction_refuses_to_escape_the_data_directory() {
    let root = scratch("traversal");
    let data_root = root.join("varapp");
    fs::create_dir_all(&data_root).unwrap();
    std::env::set_var("FLATBAK_DATA_ROOT", &data_root);

    // A canary outside the application's directory, which nothing should touch.
    let canary = root.join("canary.txt");
    fs::write(&canary, b"untouched\n").unwrap();

    let path = root.join("hostile.flatbak");
    write_archive(
        &path,
        &manifest_toml(1, true),
        &build_tar(&[
            // Legitimate content, to prove the safe entries still land.
            Entry::Dir {
                name: format!("data/{APP_ID}"),
            },
            file(&format!("data/{APP_ID}/config/good.txt"), "kept"),
            // Climbing out with `..`.
            file(&format!("data/{APP_ID}/../../canary.txt"), "OWNED"),
            // An absolute path.
            file("/etc/cron.d/evil", "OWNED"),
            // Escaping the payload prefix entirely.
            file("../../../../etc/evil", "OWNED"),
            // A symlink to somewhere absolute.
            Entry::Symlink {
                name: format!("data/{APP_ID}/passwd-link"),
                target: "/etc/passwd".to_owned(),
            },
            // A symlink climbing out of the app directory.
            Entry::Symlink {
                name: format!("data/{APP_ID}/escape-link"),
                target: "../../../canary.txt".to_owned(),
            },
            // A relative symlink that stays inside: this one is legitimate.
            Entry::Symlink {
                name: format!("data/{APP_ID}/config/inside-link"),
                target: "good.txt".to_owned(),
            },
            // Entry types FlatBak never writes.
            Entry::HardLink {
                name: format!("data/{APP_ID}/hard"),
                target: "../../../../etc/passwd".to_owned(),
            },
            // Data belonging to an application that was not selected.
            file("data/org.example.Other/secret", "not selected"),
        ]),
    );

    let backup = Backup::open(&path).expect("the hostile archive is well framed");
    let request = RestoreRequest {
        selections: vec![RestoreSelection {
            index: 0,
            install: false,
            restore_data: true,
            on_conflict: ConflictChoice::Replace,
        }],
        install_to_user: false,
        remotes_to_add: Vec::new(),
    };
    let report = reader::restore(
        &backup,
        &request,
        &Flatpak::detect(),
        &Cancel::new(),
        &mut |_| {},
    )
    .expect("hostile entries are skipped, not fatal");

    // The canary is intact and nothing was written outside the app directory.
    assert_eq!(fs::read(&canary).unwrap(), b"untouched\n");
    assert!(!root.join("etc").exists());
    assert!(!data_root.join("etc").exists());

    // The legitimate entries did land.
    let app = data_root.join(APP_ID);
    assert_eq!(fs::read(app.join("config/good.txt")).unwrap(), b"kept");
    let inside = app.join("config/inside-link");
    assert!(fs::symlink_metadata(&inside).unwrap().is_symlink());
    assert_eq!(fs::read_link(&inside).unwrap(), Path::new("good.txt"));

    // The hostile ones did not.
    assert!(!app.join("passwd-link").exists());
    assert!(!app.join("escape-link").exists());
    assert!(!app.join("hard").exists());
    // An unselected application's data is skipped without complaint.
    assert!(!data_root.join("org.example.Other").exists());

    let unsafe_count = report
        .issues
        .iter()
        .filter(|issue| matches!(issue, Issue::UnsafeEntry { .. }))
        .count();
    let rejected = report
        .issues
        .iter()
        .filter(|issue| {
            matches!(
                issue,
                Issue::UnsafeEntry { .. } | Issue::DataRestoreFailed { .. }
            )
        })
        .count();
    assert!(
        unsafe_count >= 3,
        "the three bad paths should be reported as unsafe entries: {:?}",
        report.issues
    );
    assert!(
        rejected >= 6,
        "every hostile entry should be reported: {:?}",
        report.issues
    );

    fs::remove_dir_all(&root).ok();
}
