//! Crash-safe writes of user-visible files: exports, renders, DNG conversions, smart previews and
//! XMP sidecars all go through here (the catalog's own files use [`crate::store::FsStore`]).
//!
//! **Write policy** (issue #134): what can't be recreated — originals, XMP sidecars, the catalog,
//! settings, DNGs that replace raws, smart previews (the only stand-in while an original is
//! offline), an edit copy that becomes a library photo — is written **durably**: synced to disk
//! before it counts as written. What can always be made again from the originals — exports,
//! renders, screenshots, merge previews — is written **atomically** only: a half-written file
//! never replaces a good one, but there is no per-file sync (on a USB drive or a NAS that sync
//! dominates a large export).
//!
//! - [`write_atomic`] replaces a file so that after a crash, a full disk or an unplugged drive
//!   either the old or the complete new content is there — never a truncated mix. The content
//!   goes to a new temp file in the same folder (unique name, `create_new`), is synced to disk and
//!   renamed over the target; then the folder is synced (POSIX). Any failure removes the temp
//!   file and leaves the target as it was.
//! - [`write_atomic_nosync`] is the same temp file + rename without the two syncs, for
//!   recreatable outputs only: a failed write still removes the temp file and leaves the target as
//!   it was, but after a power cut the new file may be lost (on some file systems left empty) —
//!   it is then simply exported again.
//! - [`write_new`] / [`write_new_unique`] write a file that must not exist yet: the same temp file
//!   and sync, plus a read-back check, and the file then appears under its final name **without
//!   replacing anything** (a hard link, which fails on a taken name; where the volume has no hard
//!   links, an exclusive create that claims the name, then a rename over that placeholder).
//! - [`same_file`] tells whether two paths name the same file on disk (device + inode on Unix,
//!   volume serial number + file index on Windows,
//!   resolved paths elsewhere), for "is this target an original?" checks.

use std::cell::Cell;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub use lightcraft_fs_security::canonical_names_equal;

/// A staging file and its optional private parent. A private parent prevents pre-opened
/// descriptors from observing replacement bytes before the old access controls are restored.
struct TempFile {
    path: PathBuf,
    private_dir: Option<PathBuf>,
    cleanup: File,
    published: bool,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.published && self.private_dir.is_some() {
            // Only an unpublished staging inode is touched. A successful rename removed this
            // path, so the published destination's ACL and mode are never changed by cleanup.
            let _ = lightcraft_fs_security::clear_acl(&self.cleanup);
        }
        let _ = fs::remove_file(&self.path);
        if let Some(dir) = &self.private_dir {
            let _ = fs::remove_dir(dir);
        }
    }
}

struct ReplacementSecurity {
    permissions: fs::Permissions,
    #[cfg(unix)]
    owner: (u32, u32),
    acl: lightcraft_fs_security::Acl,
}

impl ReplacementSecurity {
    fn read(path: &Path) -> io::Result<Option<Self>> {
        match fs::symlink_metadata(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
            Ok(_) => {}
        }
        Self::check_metadata(&fs::metadata(path)?)?;
        // Request the same write access as the old in-place writer, without truncation. This
        // also avoids silently replacing a read-only destination merely because its parent is writable.
        let file = OpenOptions::new().write(true).open(path)?;
        let meta = file.metadata()?;
        Self::check_metadata(&meta)?;
        Ok(Some(Self {
            permissions: meta.permissions(),
            #[cfg(unix)]
            owner: {
                use std::os::unix::fs::MetadataExt;
                (meta.uid(), meta.gid())
            },
            acl: lightcraft_fs_security::Acl::read(&file)?,
        }))
    }

    fn check_metadata(meta: &fs::Metadata) -> io::Result<()> {
        if !meta.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "replacement target is not a regular file"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o7000 != 0 {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "refusing to replace a file with special permission bits"));
            }
        }
        Ok(())
    }

    fn apply(&self, file: &File) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, fchown};
            let now = file.metadata()?;
            if (now.uid(), now.gid()) != self.owner {
                fchown(file, Some(self.owner.0), Some(self.owner.1))?;
            }
        }
        file.set_permissions(self.permissions.clone())?;
        self.acl.apply(file)
    }
}

thread_local! {
    static FAIL_AFTER: Cell<Option<u64>> = const { Cell::new(None) };
    static SYNCS: Cell<u64> = const { Cell::new(0) };
}

/// How many file and folder syncs this module made **on the calling thread** (for tests: a
/// durable write path syncs, a recreatable output doesn't).
#[doc(hidden)]
pub fn syncs_on_this_thread() -> u64 {
    SYNCS.with(Cell::get)
}

fn sync_file(f: &File) -> io::Result<()> {
    SYNCS.with(|c| c.set(c.get() + 1));
    f.sync_all()
}

/// Fault injection for tests (in any crate): while the returned guard lives, every write made by
/// this module **on the calling thread** fails once `bytes` bytes of a file were written — as a
/// full disk or an unplugged drive would.
#[doc(hidden)]
pub fn fail_writes_after(bytes: u64) -> FaultGuard {
    FAIL_AFTER.with(|c| c.set(Some(bytes)));
    FaultGuard(())
}

/// Ends a [`fail_writes_after`] when dropped.
#[doc(hidden)]
pub struct FaultGuard(());

impl Drop for FaultGuard {
    fn drop(&mut self) {
        FAIL_AFTER.with(|c| c.set(None));
    }
}

/// Counts what is written; fails past an injected limit.
struct Limited<W> {
    inner: W,
    n: u64,
    limit: Option<u64>,
}

impl<W: Write> Write for Limited<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let buf = match self.limit {
            Some(l) => {
                let room = usize::try_from(l.saturating_sub(self.n)).unwrap_or(usize::MAX);
                if room == 0 && !buf.is_empty() {
                    return Err(io::Error::other("write failed (injected fault)"));
                }
                buf.get(..room.min(buf.len())).unwrap_or(buf)
            }
            None => buf,
        };
        let k = self.inner.write(buf)?;
        self.n += k as u64;
        Ok(k)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn parent_of(path: &Path) -> &Path {
    path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

/// Make a rename or a new name in `dir` durable (POSIX). Opening a directory fails on Windows,
/// where NTFS journals the rename itself, so errors are ignored.
pub fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = sync_file(&d);
    }
}

/// A new, empty temp file next to `path` (hidden, unique in this process and folder).
fn create_temp(path: &Path, private: bool) -> io::Result<(TempFile, File)> {
    static N: AtomicU64 = AtomicU64::new(0);
    if path.file_name().is_none() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is not a file path", path.display())));
    }
    let dir = parent_of(path);
    for _ in 0..1000 {
        let staging = dir.join(format!(".lightcraft-{}-{}.tmp", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let private_dir = if private {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&staging) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
            // macOS mode bits alone do not suppress inherited ACL grants. Clear them before
            // creating any child file. A descriptor opened before this step grants no later
            // directory search permission to other users.
            let result = File::open(&staging).and_then(|d| lightcraft_fs_security::clear_acl(&d));
            if let Err(e) = result {
                let _ = fs::remove_dir(&staging);
                return Err(e);
            }
            Some(staging.clone())
        } else {
            None
        };
        let tmp = if private { staging.join("contents") } else { staging };
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => match f.try_clone() {
                Ok(cleanup) => return Ok((TempFile { path: tmp, private_dir, cleanup, published: false }, f)),
                Err(e) => {
                    let _ = fs::remove_file(&tmp);
                    if let Some(d) = &private_dir {
                        let _ = fs::remove_dir(d);
                    }
                    return Err(e);
                }
            },
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if let Some(d) = &private_dir {
                    let _ = fs::remove_dir(d);
                }
                continue;
            }
            Err(e) => {
                if let Some(d) = &private_dir {
                    let _ = fs::remove_dir(d);
                }
                return Err(e);
            }
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("no free temp file name in {}", dir.display())))
}

/// Write a temp file next to `path` with what `fill` writes, synced to disk when `sync`. On
/// failure the temp file is removed. Returns its path and length.
fn write_temp(path: &Path, sync: bool, replacing: bool, fill: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>) -> io::Result<(TempFile, u64)> {
    let security = if replacing { ReplacementSecurity::read(path)? } else { None };
    let (tmp, file) = create_temp(path, security.is_some())?;
    if let Some(security) = &security {
        security.apply(&file)?;
    }
    let r = (|| {
        let limit = FAIL_AFTER.with(Cell::get);
        let mut w = Limited { inner: io::BufWriter::with_capacity(1 << 20, file), n: 0, limit };
        fill(&mut w)?;
        w.flush()?;
        let n = w.n;
        let f = w.inner.into_inner().map_err(|e| e.into_error())?;
        if sync {
            sync_file(&f)?;
        }
        Ok(n)
    })();
    match r {
        Ok(n) => Ok((tmp, n)),
        Err(e) => Err(e),
    }
}

/// Replace (or create) `path` with `data` atomically and durably; see the module docs. The
/// folder must exist.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    write_atomic_with(path, &mut |w| w.write_all(data)).map(|_| ())
}

/// [`write_atomic`] with streamed content: `fill` writes it (an error aborts and leaves the old
/// file). Returns the length written.
pub fn write_atomic_with(path: &Path, fill: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>) -> io::Result<u64> {
    replace_with(path, true, fill)
}

/// Replace (or create) `path` with `data` atomically but **without syncing** it to disk — only for
/// outputs that can always be recreated (exports, renders, screenshots); see the module docs.
/// Anything that can't be made again goes through [`write_atomic`].
pub fn write_atomic_nosync(path: &Path, data: &[u8]) -> io::Result<()> {
    replace_with(path, false, &mut |w| w.write_all(data)).map(|_| ())
}

/// Temp file (synced when `sync`) renamed over `path`, then the folder synced when `sync`.
fn replace_with(path: &Path, sync: bool, fill: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>) -> io::Result<u64> {
    let (mut tmp, n) = write_temp(path, sync, true, fill)?;
    fs::rename(&tmp.path, path)?;
    tmp.published = true;
    if sync {
        sync_dir(parent_of(path));
    }
    Ok(n)
}

/// Write `data` as the new file `path`: never replaces an existing file (`AlreadyExists`), and
/// the file only appears once it is complete, synced and read back identical.
pub fn write_new(path: &Path, data: &[u8]) -> io::Result<()> {
    write_new_unique(&mut std::iter::once(path.to_path_buf()), data).map(|_| ())
}

/// [`write_new`] under the first free name of `candidates` (all in the same folder, e.g.
/// `IMG_1.dng`, `IMG_1-2.dng`, …): the file is written once and then given the first name that
/// is not taken. Returns that name; `AlreadyExists` when every candidate is taken.
pub fn write_new_unique(candidates: &mut dyn Iterator<Item = PathBuf>, data: &[u8]) -> io::Result<PathBuf> {
    let mut candidates = candidates.peekable();
    let Some(first) = candidates.peek().cloned() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no file name to write"));
    };
    let (mut tmp, _) = write_temp(&first, true, false, &mut |w| w.write_all(data))?;
    let r = (|| {
        // Kept (issue #134 point 4): these files replace raws (Convert / Copy as DNG) or become
        // library photos (merges). On a local disk the read usually comes from the cache, but on a
        // network volume it can go back to the server, and next to encoding a DNG it costs little.
        if !same_content(&tmp.path, data)? {
            return Err(io::Error::other("the written file reads back different (failing drive or connection?)"));
        }
        for path in candidates.by_ref().take(100_000) {
            match publish_new(&tmp.path, &path) {
                Ok(()) => {
                    tmp.published = true;
                    sync_dir(parent_of(&path));
                    return Ok(path);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{}: no free file name", first.display())))
    })();
    // after a hard link the temp name is a second name of the new file; after a rename it's gone
    drop(tmp);
    r
}

/// Give the complete temp file `tmp` the name `path`, unless that is taken.
fn publish_new(tmp: &Path, path: &Path) -> io::Result<()> {
    match fs::hard_link(tmp, path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
        // no hard links on this volume (FAT/exFAT, some shares)
        Err(_) => publish_new_by_rename(tmp, path, &|| {}),
    }
}

/// Fallback of [`publish_new`]: reserve the name with an exclusive create (atomic, so a file that
/// appears at the same time is never replaced), then rename the complete temp file over our own
/// empty placeholder. `before_claim` runs just before the name is claimed (a test hook).
fn publish_new_by_rename(tmp: &Path, path: &Path, before_claim: &dyn Fn()) -> io::Result<()> {
    before_claim();
    OpenOptions::new().write(true).create_new(true).open(path)?;
    if let Err(e) = fs::rename(tmp, path) {
        let _ = fs::remove_file(path);
        return Err(e);
    }
    Ok(())
}

/// Does the file at `path` hold exactly `data`?
fn same_content(path: &Path, data: &[u8]) -> io::Result<bool> {
    use std::io::Read;
    let mut f = File::open(path)?;
    if f.metadata()?.len() != data.len() as u64 {
        return Ok(false);
    }
    let mut buf = vec![0u8; 1 << 20];
    let mut at = 0usize;
    loop {
        let k = match f.read(&mut buf) {
            Ok(0) => return Ok(at == data.len()),
            Ok(k) => k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if data.get(at..at + k) != buf.get(..k) {
            return Ok(false);
        }
        at += k;
    }
}

/// Do `a` and `b` name the same existing file (through hard links, symlinks, `..`,
/// case-insensitive names)?
pub fn same_file(a: &Path, b: &Path) -> bool {
    compare_files(a, b, false).unwrap_or(false)
}

/// [`same_file`] for a check that must not let an unknown identity pass: `Err` when the metadata
/// of either path cannot be read for a reason other than the path not existing (for example a
/// folder it cannot enter), or when both paths exist with the same length but the file identity
/// of either cannot be read (on Windows, when either file cannot be opened or reports file index 0). A path that does not
/// exist is still `Ok(false)`.
pub fn same_file_strict(a: &Path, b: &Path) -> io::Result<bool> {
    compare_files(a, b, true)
}

fn compare_files(a: &Path, b: &Path, strict: bool) -> io::Result<bool> {
    let missing = |e: &io::Error| matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory);
    let (ma, mb) = match (fs::metadata(a), fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => (ma, mb),
        (Err(e), _) | (_, Err(e)) if strict && !missing(&e) => return Err(e),
        _ => return Ok(false),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(ma.dev() == mb.dev() && ma.ino() == mb.ino())
    }
    #[cfg(not(unix))]
    {
        if ma.len() != mb.len() {
            return Ok(false);
        }
        // A hard link has its own path but shares the volume serial number and file index with
        // its original. A file index of 0 produces an error in strict mode. Lenient mode compares resolved paths.
        // On ReFS two files can share the 64-bit file index, so the lengths are compared first.
        #[cfg(windows)]
        {
            match file_ids(a, b).and_then(|(ia, ib)| same_file_id(ia, ib, strict)) {
                Ok(true) => return Ok(true),
                Ok(_) => {}
                Err(e) if strict => return Err(e),
                Err(_) => {}
            }
        }
        Ok(match (fs::canonicalize(a), fs::canonicalize(b)) {
            (Ok(x), Ok(y)) => x == y || x.to_string_lossy().to_lowercase() == y.to_string_lossy().to_lowercase(),
            _ => false,
        })
    }
}

/// The volume serial number and file index of `a` and `b`, read while both files are open.
#[cfg(windows)]
fn file_ids(a: &Path, b: &Path) -> io::Result<((u64, u64), (u64, u64))> {
    use winapi_util::{Handle, file::information};
    let (ha, hb) = (Handle::from_path_any(a)?, Handle::from_path_any(b)?);
    let (ia, ib) = (information(&ha)?, information(&hb)?);
    Ok(((ia.volume_serial_number(), ia.file_index()), (ib.volume_serial_number(), ib.file_index())))
}

/// The comparison uses each file's volume serial number and file index. Strict mode returns an error for file index 0.
#[cfg(any(windows, test))]
fn same_file_id(a: (u64, u64), b: (u64, u64), strict: bool) -> io::Result<bool> {
    if strict && (a.1 == 0 || b.1 == 0) {
        return Err(io::Error::other("file identity is unavailable"));
    }
    Ok(a.1 != 0 && b.1 != 0 && a == b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-safe-file-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        v.sort();
        v
    }

    #[test]
    fn fallback_publish_never_replaces_a_file_that_appears_at_the_same_time() {
        let d = temp("nolink-race");
        let tmp = d.join(".tmp-new");
        let dest = d.join("IMG_1.dng");
        fs::write(&tmp, b"new").unwrap();
        let e = publish_new_by_rename(&tmp, &dest, &|| fs::write(&dest, b"other writer").unwrap()).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&dest).unwrap(), b"other writer");
        fs::remove_file(&dest).unwrap();
        publish_new_by_rename(&tmp, &dest, &|| {}).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"new");
        assert!(!tmp.exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let d = temp("replace");
        let p = d.join("out.jpg");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two two");
        assert_eq!(names(&d), vec!["out.jpg"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failure_mid_write_keeps_the_old_file() {
        let d = temp("midwrite");
        let p = d.join("photo.jpg");
        fs::write(&p, b"the original bytes").unwrap();
        {
            let _f = fail_writes_after(4);
            let e = write_atomic(&p, &[7u8; 10_000]).unwrap_err();
            assert!(e.to_string().contains("injected"), "{e}");
            assert!(write_new(&d.join("new.dng"), &[1u8; 100]).is_err());
        }
        assert_eq!(fs::read(&p).unwrap(), b"the original bytes", "the target is untouched");
        assert_eq!(names(&d), vec!["photo.jpg"], "no temp file and no partial new file");
        // a fill error aborts the same way
        let r = write_atomic_with(&p, &mut |w| {
            w.write_all(b"half")?;
            Err(io::Error::other("encoder failed"))
        });
        assert!(r.is_err());
        assert_eq!(fs::read(&p).unwrap(), b"the original bytes");
        assert_eq!(names(&d), vec!["photo.jpg"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn recreatable_outputs_are_atomic_without_a_sync() {
        let d = temp("nosync");
        let p = d.join("export.jpg");
        let before = syncs_on_this_thread();
        write_atomic_nosync(&p, b"one").unwrap();
        write_atomic_nosync(&p, b"two two").unwrap();
        assert_eq!(syncs_on_this_thread(), before, "no file or folder sync");
        assert_eq!(fs::read(&p).unwrap(), b"two two");
        assert_eq!(names(&d), vec!["export.jpg"], "no temp file left");
        // still atomic: a failure part-way leaves the previous file and no temp file
        {
            let _f = fail_writes_after(3);
            assert!(write_atomic_nosync(&p, &[9u8; 10_000]).is_err());
        }
        assert_eq!(fs::read(&p).unwrap(), b"two two");
        assert_eq!(names(&d), vec!["export.jpg"]);
        // the durable writers do sync (the file; the folder too where it can be opened)
        let before = syncs_on_this_thread();
        write_atomic(&p, b"three").unwrap();
        let mid = syncs_on_this_thread();
        assert!(mid > before);
        write_new(&d.join("new.dng"), b"dng").unwrap();
        assert!(syncs_on_this_thread() > mid);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn new_files_never_replace_anything() {
        let d = temp("new");
        let p = d.join("a.dng");
        fs::write(&p, b"keep").unwrap();
        assert_eq!(write_new(&p, b"other").unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&p).unwrap(), b"keep");
        let mut c = (1..).map(|k| if k == 1 { d.join("a.dng") } else { d.join(format!("a-{k}.dng")) });
        let got = write_new_unique(&mut c, b"fresh").unwrap();
        assert_eq!(got, d.join("a-2.dng"));
        assert_eq!(fs::read(&got).unwrap(), b"fresh");
        assert_eq!(names(&d), vec!["a-2.dng", "a.dng"], "the temp file is gone");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn same_file_sees_a_hard_link_under_another_name() {
        let d = temp("same-link");
        let p = d.join("IMG_1.jpg");
        fs::write(&p, b"original").unwrap();
        let linked = d.join("linked.png");
        match fs::hard_link(&p, &linked) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => panic!("{}: {e}", linked.display()),
            // no hard links on this volume (FAT/exFAT, some shares), as in `publish_new`
            Err(e) => {
                let _ = fs::remove_dir_all(&d);
                lightcraft_testkit::skip!(System, "no hard links in {}: {e}", d.display());
                return;
            }
        }
        fs::write(d.join("copy.png"), b"original").unwrap();
        assert!(same_file(&p, &linked), "a hard link is the same file");
        assert!(same_file(&linked, &p));
        assert!(!same_file(&p, &d.join("copy.png")), "a copy with the same content and length is another file");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn same_file_sees_through_other_spellings() {
        let d = temp("same");
        let p = d.join("x.jpg");
        fs::write(&p, b"x").unwrap();
        fs::write(d.join("y.jpg"), b"x").unwrap();
        fs::create_dir_all(d.join("sub")).unwrap();
        assert!(same_file(&p, &d.join("sub/../x.jpg")));
        assert!(!same_file(&p, &d.join("y.jpg")));
        assert!(!same_file(&p, &d.join("missing.jpg")));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn strict_comparison_reports_a_path_it_cannot_query() {
        let d = temp("strict-query");
        let p = d.join("x.jpg");
        fs::write(&p, b"x").unwrap();
        // A NUL byte makes the metadata lookup fail with an error other than "not found" on
        // every platform, as a folder the process cannot enter does.
        let unreadable = Path::new("IMG_1\0.jpg");
        assert!(same_file_strict(unreadable, &p).is_err());
        assert!(same_file_strict(&p, unreadable).is_err());
        assert!(!same_file(unreadable, &p), "the lenient comparison still answers false");
        assert!(!same_file_strict(&d.join("missing.jpg"), &p).unwrap(), "a missing original is another file");
        assert!(!same_file_strict(&p.join("under-a-file.jpg"), &p).unwrap());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_zero_file_index_is_not_a_file_identity() {
        for strict in [false, true] {
            assert!(same_file_id((7, 42), (7, 42), strict).unwrap());
            assert!(!same_file_id((7, 42), (7, 43), strict).unwrap());
            assert!(!same_file_id((7, 42), (8, 42), strict).unwrap());
        }
        for (a, b) in [((7, 0), (7, 0)), ((0, 0), (0, 0)), ((7, 42), (7, 0)), ((7, 0), (7, 42))] {
            assert!(same_file_id(a, b, true).is_err(), "strict comparison must reject an unavailable identity: {a:?}, {b:?}");
            assert!(!same_file_id(a, b, false).unwrap(), "lenient comparison can fall back to resolved paths");
        }
    }
    #[cfg(unix)]
    #[test]
    fn replacement_is_private_and_has_original_ownership_before_fill() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let d = temp("private-fill");
        let p = d.join("private.txt");
        fs::write(&p, b"old private bytes").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
        let before = fs::metadata(&p).unwrap();
        write_atomic_with(&p, &mut |w| {
            let staging = fs::read_dir(&d).unwrap().flatten().find(|e| e.file_type().unwrap().is_dir()).unwrap().path();
            assert_eq!(fs::metadata(&staging).unwrap().permissions().mode() & 0o777, 0o700);
            let file = fs::metadata(staging.join("contents")).unwrap();
            assert_eq!(file.permissions().mode() & 0o777, 0o600);
            assert_eq!((file.uid(), file.gid()), (before.uid(), before.gid()));
            assert_eq!(fs::read(&p).unwrap(), b"old private bytes");
            w.write_all(b"new private bytes")
        })
        .unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"new private bytes");
        assert_eq!(names(&d), vec!["private.txt"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn special_permission_bits_fail_before_fill_without_touching_the_destination() {
        use std::os::unix::fs::PermissionsExt;
        let d = temp("special-mode");
        let p = d.join("special.txt");
        fs::write(&p, b"keep special").unwrap();
        // Sticky is retained on ordinary user-owned files even where the OS strips set-id.
        let mode = 0o1600;
        fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o7777, mode);
        let mut filled = false;
        assert_eq!(
            write_atomic_with(&p, &mut |_| {
                filled = true;
                Ok(())
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(!filled);
        assert_eq!(fs::read(&p).unwrap(), b"keep special");
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o7777, mode);
        assert_eq!(names(&d), vec!["special.txt"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_directory_is_rejected_before_fill_without_staging_artifacts() {
        let d = temp("directory-target");
        let p = d.join("directory");
        fs::create_dir(&p).unwrap();
        let mut filled = false;
        assert_eq!(
            write_atomic_with(&p, &mut |_| {
                filled = true;
                Ok(())
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(!filled);
        assert!(p.is_dir());
        assert_eq!(names(&d), vec!["directory"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_replacement_of_an_ordinary_symlink_keeps_its_referent_unchanged() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let d = temp("ordinary-link");
        let source = d.join("source.txt");
        let alias = d.join("alias.txt");
        fs::write(&source, b"keep referent").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&source, &alias).unwrap();
        write_atomic(&alias, b"export").unwrap();
        assert!(!fs::symlink_metadata(&alias).unwrap().is_symlink());
        assert_eq!(fs::read(&alias).unwrap(), b"export");
        assert_eq!(fs::read(&source).unwrap(), b"keep referent");
        assert_eq!(fs::metadata(&alias).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn staging_drops_inherited_grants_but_new_files_retain_them() {
        fn acl(path: &Path) -> Vec<String> {
            let out = std::process::Command::new("/bin/ls").arg("-lde").arg(path).output().unwrap();
            assert!(out.status.success());
            String::from_utf8(out.stdout).unwrap().lines().skip(1).map(str::trim).map(str::to_owned).collect()
        }
        let d = temp("staging-acl");
        let p = d.join("existing.txt");
        fs::write(&p, b"old").unwrap();
        let out = std::process::Command::new("/bin/chmod")
            .args(["+a", "group:everyone allow read,write,file_inherit,directory_inherit"])
            .arg(&d)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        write_atomic_with(&p, &mut |w| {
            let staging = fs::read_dir(&d).unwrap().flatten().find(|e| e.file_type().unwrap().is_dir()).unwrap().path();
            assert!(acl(&staging).is_empty(), "staging directory must not inherit access grants");
            assert!(acl(&staging.join("contents")).is_empty());
            w.write_all(b"replacement")
        })
        .unwrap();
        assert!(acl(&p).is_empty());
        let new = d.join("new.txt");
        write_new(&new, b"new").unwrap();
        assert!(
            acl(&new).iter().any(|line| line.contains("inherited") && line.contains("allow")),
            "publishing and cleanup must keep a new file's inherited ACL"
        );
        assert_eq!(names(&d), vec!["existing.txt", "new.txt"]);
        let _ = fs::remove_dir_all(&d);
    }
}
