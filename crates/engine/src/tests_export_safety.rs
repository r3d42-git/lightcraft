//! File exports must protect catalog sources and preserve an existing destination's access rules.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;

use crate::Session;

const COMMANDS: [&str; 2] = ["keyword.export", "mask.exportEffects"];

struct Fixture {
    dir: PathBuf,
    source: PathBuf,
    original: Vec<u8>,
    session: Session,
}

impl Fixture {
    fn new() -> Self {
        Self::with_name("IMG_1.jpg")
    }

    fn with_name(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lightcraft-export-safety-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        let source = dir.join(name);
        let image = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [(x * 7) as u8, (y * 9) as u8, 90, 255]);
        let original = crate::export::encode_image(&image, &crate::export::ExportOptions::default()).unwrap();
        std::fs::write(&source, &original).unwrap();
        let mut session = Session::new().with_fs();
        session.execute("library.import", &json!({"paths": [source.to_string_lossy()]})).unwrap();
        session.execute("keyword.import", &json!({"text": "Travel\n\tLisbon\n"})).unwrap();
        session.mask_effects.push(crate::cmd::mask_effects::MaskEffect { name: "Gentle light".into(), ..Default::default() });
        Self { dir, source, original, session }
    }

    fn export(&mut self, command: &str, path: &Path) -> crate::Result<serde_json::Value> {
        self.session.execute(command, &json!({"path": path.to_string_lossy()}))
    }

    fn assert_original(&self) {
        assert_eq!(std::fs::read(&self.source).unwrap(), self.original);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn keyword_and_mask_exports_refuse_existing_catalog_originals_and_both_sidecars() {
    let mut f = Fixture::new();
    let replaced = f.source.with_extension("xmp");
    let appended = f.dir.join("IMG_1.jpg.xmp");
    std::fs::write(&replaced, b"replaced-extension sidecar").unwrap();
    std::fs::write(&appended, b"appended-extension sidecar").unwrap();
    for command in COMMANDS {
        for path in [&f.source.clone(), &replaced, &appended] {
            assert!(f.export(command, path).is_err(), "{command} accepted {}", path.display());
            f.assert_original();
            assert_eq!(std::fs::read(&replaced).unwrap(), b"replaced-extension sidecar");
            assert_eq!(std::fs::read(&appended).unwrap(), b"appended-extension sidecar");
        }
    }
}

#[cfg(unix)]
#[test]
fn keyword_and_mask_exports_refuse_symlink_and_hardlink_aliases_of_catalog_files() {
    use std::os::unix::fs::symlink;
    let mut f = Fixture::new();
    let replaced = f.source.with_extension("xmp");
    let appended = f.dir.join("IMG_1.jpg.xmp");
    std::fs::write(&replaced, b"first sidecar").unwrap();
    std::fs::write(&appended, b"second sidecar").unwrap();
    for (i, source) in [f.source.clone(), replaced.clone(), appended.clone()].iter().enumerate() {
        let symbolic = f.dir.join(format!("symbolic-{i}.txt"));
        let hard = f.dir.join(format!("hard-{i}.txt"));
        symlink(source, &symbolic).unwrap();
        std::fs::hard_link(source, &hard).unwrap();
        for command in COMMANDS {
            for target in [&symbolic, &hard] {
                assert!(f.export(command, target).is_err(), "{command} accepted {}", target.display());
            }
        }
    }
    f.assert_original();
    assert_eq!(std::fs::read(replaced).unwrap(), b"first sidecar");
    assert_eq!(std::fs::read(appended).unwrap(), b"second sidecar");
}

#[test]
fn keyword_and_mask_exports_reserve_absent_sidecars_through_case_and_parent_components() {
    let mut f = Fixture::new();
    std::fs::create_dir(f.dir.join("sub")).unwrap();
    for command in COMMANDS {
        for name in ["IMG_1.xmp", "IMG_1.jpg.xmp", "IMG_1.XmP", "IMG_1.JPG.XMP", "sub/../IMG_1.xmp", "sub/../IMG_1.jpg.xmp"] {
            let target = f.dir.join(name);
            assert!(f.export(command, &target).is_err(), "{command} accepted {}", target.display());
            assert!(!target.exists(), "a refused absent sidecar was created");
            f.assert_original();
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn keyword_and_mask_exports_reserve_absent_sidecars_through_unicode_normalization() {
    let mut f = Fixture::with_name("caf\u{e9}.jpg");
    let source = f.source.clone();
    assert!(
        lightcraft_catalog::safe_file::same_file(&source, &f.dir.join("cafe\u{301}.jpg")),
        "fixture needs a normalization-insensitive macOS filesystem"
    );
    for command in COMMANDS {
        for name in ["cafe\u{301}.xmp", "cafe\u{301}.jpg.xmp"] {
            let target = f.dir.join(name);
            assert!(f.export(command, &target).is_err(), "{command} accepted {}", target.display());
            assert!(!target.exists());
            assert_eq!(std::fs::read(&source).unwrap(), f.original);
        }
    }
    std::fs::remove_file(&source).unwrap();
    for command in COMMANDS {
        assert!(f.export(command, &f.dir.join("cafe\u{301}.xmp")).is_err(), "offline originals still reserve their sidecars");
        f.export(command, &f.dir.join("cafe.xmp")).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn keyword_and_mask_exports_reserve_absent_sidecars_through_directory_symlinks() {
    let mut f = Fixture::new();
    let alias = f.dir.join("directory-alias");
    std::os::unix::fs::symlink(&f.dir, &alias).unwrap();
    for command in COMMANDS {
        for name in ["IMG_1.xmp", "IMG_1.jpg.xmp", "IMG_1.XMP", "IMG_1.JPG.XMP"] {
            let target = alias.join(name);
            assert!(f.export(command, &target).is_err(), "{command} accepted {}", target.display());
            assert!(!target.exists());
        }
    }
    f.assert_original();
}

#[cfg(target_os = "macos")]
#[test]
fn keyword_and_mask_exports_refuse_existing_unicode_aliases_after_a_normalization_only_rename() {
    let mut f = Fixture::with_name("caf\u{e9}.jpg");
    let renamed = f.dir.join("cafe\u{301}.jpg");
    std::fs::rename(&f.source, &renamed).unwrap();
    let replaced = f.dir.join("cafe\u{301}.xmp");
    let appended = f.dir.join("cafe\u{301}.jpg.xmp");
    std::fs::write(&replaced, b"keep first sidecar").unwrap();
    std::fs::write(&appended, b"keep second sidecar").unwrap();
    for command in COMMANDS {
        for target in [&renamed, &replaced, &appended] {
            assert!(f.export(command, target).is_err(), "{command} accepted {}", target.display());
        }
    }
    f.assert_original();
    assert_eq!(std::fs::read(replaced).unwrap(), b"keep first sidecar");
    assert_eq!(std::fs::read(appended).unwrap(), b"keep second sidecar");
}

#[test]
fn keyword_and_mask_exports_allow_the_same_sidecar_name_in_another_folder() {
    let mut f = Fixture::new();
    let other = f.dir.join("other");
    std::fs::create_dir(&other).unwrap();
    for command in COMMANDS {
        for name in ["IMG_1.xmp", "IMG_1.jpg.xmp"] {
            let target = other.join(name);
            f.export(command, &target).unwrap();
            assert!(!std::fs::read(&target).unwrap().is_empty());
        }
    }
    f.assert_original();
    assert!(!f.source.with_extension("xmp").exists());
    assert!(!f.dir.join("IMG_1.jpg.xmp").exists());
}

#[test]
fn keyword_and_mask_exports_keep_normal_output_and_parent_creation_contracts() {
    let mut f = Fixture::new();
    let text = f.session.execute("keyword.export", &json!({})).unwrap();
    assert_eq!(text["text"], "Travel\n\tLisbon\n");
    assert_eq!(text["keywords"], 2);
    assert_eq!(text["captureOneRefuses"], json!([]));
    assert_eq!(text["unwritable"], json!([]));
    let keywords = f.dir.join("keywords.txt");
    let result = f.export("keyword.export", &keywords).unwrap();
    assert_eq!(result["path"], keywords.to_string_lossy().as_ref());
    assert!(result.get("text").is_none());
    assert_eq!(std::fs::read_to_string(&keywords).unwrap(), text["text"].as_str().unwrap());
    let missing_keywords = f.dir.join("missing-keywords/nested/keywords.txt");
    assert!(f.export("keyword.export", &missing_keywords).is_err());
    assert!(!f.dir.join("missing-keywords").exists());
    let effects = f.dir.join("missing-effects/nested/effects");
    let result = f.export("mask.exportEffects", &effects).unwrap();
    let actual = effects.with_extension("lceffect");
    assert_eq!(result["path"], actual.to_string_lossy().as_ref());
    assert_eq!(result["count"], 1);
    let output: serde_json::Value = serde_json::from_slice(&std::fs::read(actual).unwrap()).unwrap();
    assert_eq!(output["version"], 2);
    assert_eq!(output["effects"][0]["name"], "Gentle light");
    f.assert_original();
}

#[cfg(unix)]
#[test]
fn keyword_and_mask_exports_preserve_private_mode_and_previous_bytes_on_failed_replacement() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let mut f = Fixture::new();
    for (i, command) in COMMANDS.iter().enumerate() {
        let parent = f.dir.join(format!("replacement-{i}"));
        std::fs::create_dir(&parent).unwrap();
        let target = parent.join("export.txt");
        std::fs::write(&target, b"private previous export").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        let previous_metadata = std::fs::metadata(&target).unwrap();
        let before_entries: Vec<_> = std::fs::read_dir(&parent).unwrap().map(|e| e.unwrap().file_name()).collect();
        {
            let _fault = lightcraft_catalog::safe_file::fail_writes_after(4);
            assert!(f.export(command, &target).is_err(), "{command} must report an interrupted write");
        }
        assert_eq!(std::fs::read(&target).unwrap(), b"private previous export");
        assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777, 0o600);
        let after_entries: Vec<_> = std::fs::read_dir(&parent).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(after_entries, before_entries, "failed export leaves no staging file or directory");
        f.export(command, &target).unwrap();
        let written = std::fs::read(&target).unwrap();
        assert_ne!(written, b"private previous export");
        if *command == "keyword.export" {
            assert_eq!(written, b"Travel\n\tLisbon\n");
        } else {
            assert_eq!(serde_json::from_slice::<serde_json::Value>(&written).unwrap()["effects"][0]["name"], "Gentle light");
        }
        assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777, 0o600);
        let metadata = std::fs::metadata(&target).unwrap();
        assert_eq!((metadata.uid(), metadata.gid()), (previous_metadata.uid(), previous_metadata.gid()));
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 1, "successful export leaves no staging artifacts");
    }
}

#[cfg(target_os = "macos")]
fn acl_lines(path: &Path) -> Vec<String> {
    let output = std::process::Command::new("/bin/ls").arg("-lde").arg(path).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().lines().skip(1).map(str::trim).map(str::to_owned).collect()
}

#[cfg(target_os = "macos")]
fn add_acl(path: &Path, entry: &str) {
    let output = std::process::Command::new("/bin/chmod").arg("+a").arg(entry).arg(path).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

#[cfg(target_os = "macos")]
#[test]
fn keyword_and_mask_exports_preserve_an_existing_deny_read_acl() {
    let mut f = Fixture::new();
    for (i, command) in COMMANDS.iter().enumerate() {
        let target = f.dir.join(format!("denied-{i}.txt"));
        std::fs::write(&target, b"previous export").unwrap();
        add_acl(&target, "group:everyone deny read");
        let before = acl_lines(&target);
        assert!(
            before
                .iter()
                .any(|line| line.contains("deny read") && (line.contains("everyone") || line.contains("ABCDEFAB-CDEF-ABCD-EFAB-CDEF0000000C"))),
            "{before:?}"
        );
        f.export(command, &target).unwrap();
        assert_eq!(acl_lines(&target), before, "{command} lost the existing deny ACL");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn keyword_and_mask_exports_do_not_broaden_an_existing_file_with_parent_inherited_acl() {
    let mut f = Fixture::new();
    for (i, command) in COMMANDS.iter().enumerate() {
        let parent = f.dir.join(format!("inherited-{i}"));
        std::fs::create_dir(&parent).unwrap();
        let target = parent.join("export.txt");
        std::fs::write(&target, b"previous export").unwrap();
        let before = acl_lines(&target);
        add_acl(&parent, "group:everyone allow read,write,file_inherit,directory_inherit");
        // Prove this fixture really gives newly created files an inherited grant.
        let probe = parent.join("inheritance-probe.txt");
        std::fs::write(&probe, b"probe").unwrap();
        let inherited = acl_lines(&probe);
        assert!(
            inherited.iter().any(|line| line.contains("inherited")
                && line.contains("allow")
                && (line.contains("everyone") || line.contains("ABCDEFAB-CDEF-ABCD-EFAB-CDEF0000000C"))),
            "{inherited:?}"
        );
        std::fs::remove_file(&probe).unwrap();
        f.export(command, &target).unwrap();
        assert_eq!(acl_lines(&target), before, "{command} broadened access through the parent ACL");
    }
}
