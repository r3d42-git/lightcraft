# File security metadata

This dependency-free L0 helper owns the small, safe API around macOS extended ACLs.
`Acl::read` captures an ACL from an open descriptor; `apply` replaces the receiving
descriptor's ACL, including removing inherited entries absent from the snapshot.
`clear_acl` makes a staging directory private before any content is created inside.
Errors propagate rather than falling back to an ACL that might grant more access.
Filesystems that explicitly report ACLs unsupported carry no macOS extended ACL.

Only this module's macOS implementation uses unsafe code, to call documented
libSystem functions from `sys/acl.h`. Allocations are owned and released once;
borrowed `File` handles keep descriptors alive throughout each call. There are no
C/C++ dependencies, external commands or new registry dependencies. A second small
CoreFoundation boundary compares canonically equivalent Unicode file names on
macOS so absent sidecars cannot be created using an alternate spelling. It retains
diacritics and does not use the user's locale. Owned strings are released once.

The catalog's atomic writer separately preserves POSIX owner/group and rwx mode.
Native ACL handling on other operating systems is not supplied by this macOS helper.
Engine export regression tests exercise native ACL preservation and inheritance on
macOS. Run `cargo test -p lightcraft-engine tests_export_safety` there.
