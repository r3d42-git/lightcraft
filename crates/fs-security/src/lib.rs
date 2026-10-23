//! File ACL snapshots used before publishing an atomic replacement.
//!
//! macOS extended ACLs are separate from POSIX mode bits. Keep the small libSystem boundary
//! here; callers retain safe `File` handles and never pass raw pointers. Other platforms have
//! no macOS ACL to transfer. POSIX ownership and mode handling live in the caller.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::{fs::File, io};

/// An owned snapshot of a file's macOS extended ACL, including an empty ACL.
pub struct Acl {
    #[cfg(target_os = "macos")]
    inner: macos::Acl,
}

impl Acl {
    /// Read the ACL of this already-open file. Unsupported filesystems have no extended ACL.
    pub fn read(file: &File) -> io::Result<Self> {
        #[cfg(target_os = "macos")]
        return Ok(Self { inner: macos::Acl::read(file)? });
        #[cfg(not(target_os = "macos"))]
        {
            let _ = file;
            Ok(Self {})
        }
    }

    /// Replace the destination's ACL, removing inherited entries absent from the snapshot.
    pub fn apply(&self, file: &File) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        return self.inner.apply(file);
        #[cfg(not(target_os = "macos"))]
        {
            let _ = file;
            Ok(())
        }
    }
}

/// Remove an inherited macOS ACL before putting private staging files in a directory.
pub fn clear_acl(file: &File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    return macos::Acl::empty()?.apply(file);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = file;
        Ok(())
    }
}

/// Compare already case-folded names, including canonical Unicode equivalence on macOS.
/// This reserves absent sidecars under the alternate spellings accepted by APFS/HFS+.
/// Accents are significant; this does not perform a locale-dependent comparison.
pub fn canonical_names_equal(a: &str, b: &str) -> io::Result<bool> {
    if a == b {
        return Ok(true);
    }
    #[cfg(target_os = "macos")]
    if !a.is_ascii() || !b.is_ascii() {
        return macos::canonical_names_equal(a, b);
    }
    Ok(false)
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos {
    use super::*;
    use std::{ffi::c_void, os::fd::AsRawFd, ptr::NonNull};

    unsafe extern "C" {
        // libSystem, documented in the macOS SDK's sys/acl.h; acl_t is an opaque pointer.
        fn acl_get_fd(fd: i32) -> *mut c_void;
        fn acl_init(count: i32) -> *mut c_void;
        fn acl_set_fd(fd: i32, acl: *mut c_void) -> i32;
        fn acl_free(acl: *mut c_void) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCharacters(allocator: *const c_void, chars: *const u16, count: isize) -> *mut c_void;
        fn CFStringCompare(a: *const c_void, b: *const c_void, flags: usize) -> isize;
        fn CFRelease(value: *const c_void);
    }

    struct NativeString(NonNull<c_void>);

    impl NativeString {
        fn new(value: &str) -> io::Result<Self> {
            let utf16: Vec<u16> = value.encode_utf16().collect();
            let count = isize::try_from(utf16.len()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file name too long"))?;
            // SAFETY: the UTF-16 buffer is valid for count elements during this copying call.
            // A null allocator means the system default. The returned string is owned below.
            let raw = unsafe { CFStringCreateWithCharacters(std::ptr::null(), utf16.as_ptr(), count) };
            NonNull::new(raw).map(Self).ok_or_else(|| io::Error::other("could not compare Unicode file names"))
        }
    }

    impl Drop for NativeString {
        fn drop(&mut self) {
            // SAFETY: one owner releases the string returned by the Create function.
            unsafe { CFRelease(self.0.as_ptr()) };
        }
    }

    pub fn canonical_names_equal(a: &str, b: &str) -> io::Result<bool> {
        let a = NativeString::new(a)?;
        let b = NativeString::new(b)?;
        // SAFETY: both owned CFStrings remain valid. kCFCompareNonliteral (16) compares
        // canonical forms, without ignoring diacritics or using the user's locale.
        Ok(unsafe { CFStringCompare(a.0.as_ptr(), b.0.as_ptr(), 16) } == 0)
    }

    // A snapshot returned by get_fd must never be silently discarded on a receiving
    // filesystem without ACL support (e.g. a symlink whose referent is on another volume).
    pub struct Acl(NonNull<c_void>, bool);

    impl Acl {
        pub fn read(file: &File) -> io::Result<Self> {
            // SAFETY: a borrowed File keeps its descriptor open for the duration of the call.
            // acl_get_fd returns an independently allocated ACL, owned by this wrapper.
            let raw = unsafe { acl_get_fd(file.as_raw_fd()) };
            match NonNull::new(raw) {
                Some(ptr) => Ok(Self(ptr, true)),
                None => {
                    let error = io::Error::last_os_error();
                    // macOS reports ENOENT for an existing descriptor with no extended ACL.
                    // The descriptor remains open, so this is not a missing source pathname.
                    if matches!(error.kind(), io::ErrorKind::Unsupported | io::ErrorKind::NotFound) { Self::empty() } else { Err(error) }
                }
            }
        }

        pub fn empty() -> io::Result<Self> {
            // SAFETY: zero is a valid ACL capacity. The returned allocation is owned until Drop.
            let raw = unsafe { acl_init(0) };
            NonNull::new(raw).map(|p| Self(p, false)).ok_or_else(io::Error::last_os_error)
        }

        pub fn apply(&self, file: &File) -> io::Result<()> {
            // SAFETY: both the descriptor and the ACL allocation stay alive for this call.
            // acl_set_fd copies the entries and retains neither argument.
            if unsafe { acl_set_fd(file.as_raw_fd(), self.0.as_ptr()) } == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            // Only a known-empty ACL may be ignored on a filesystem without ACL support.
            // A captured ACL must be retained, including for cross-volume symlink destinations.
            if error.kind() == io::ErrorKind::Unsupported && !self.1 { Ok(()) } else { Err(error) }
        }
    }

    impl Drop for Acl {
        fn drop(&mut self) {
            // SAFETY: exactly one owner frees the valid allocation returned by get_fd/init.
            let _ = unsafe { acl_free(self.0.as_ptr()) };
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn canonical_names_keep_accents_and_unrelated_names_distinct() {
        assert!(super::canonical_names_equal("caf\u{e9}.xmp", "cafe\u{301}.xmp").unwrap());
        assert!(!super::canonical_names_equal("caf\u{e9}.xmp", "cafe.xmp").unwrap());
        assert!(!super::canonical_names_equal("private.xmp", "other.xmp").unwrap());
    }
}
