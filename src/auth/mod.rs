//! Authorization for mirror-fs.
//!
//! The filesystem does not reason about a concrete access-control policy
//! (POSIX bits, ACLs, …). It states what it wants to do as a semantic
//! [`Access`] intent and delegates the decision to an [`Authorizer`].
//!
//! [`PosixAuthorizer`](posix::PosixAuthorizer) is the concrete,
//! POSIX rwx/sticky-bit implementation. Swapping it for another policy only
//! affects this module.

use nfs_mamont::auth::Credential;
use nfs_mamont::vfs::access;
use nfs_mamont::vfs::file;

pub mod posix;

/// A resolved caller identity, produced by [`Authorizer::map_credentials`] and
/// consumed by the rest of the authorization interface. Downstream checks never
/// touch the raw [`Credential`] again.
#[derive(Debug, Clone)]
pub struct Credentials {
    /// Un-squashed effective uid.
    pub uid: u32,
    /// Un-squashed effective gid.
    pub gid: u32,
    /// Supplementary group ids.
    pub groups: Vec<u32>,
    /// True only for a surviving (un-squashed) UID 0. Decided here, once.
    pub privileged: bool,
}

/// A filesystem access intent, decoupled from any particular policy.
///
/// The filesystem expresses *what* it needs as an operation, not *how* any
/// policy would grant it. Mapping the intent to policy-specific rights
/// (e.g. POSIX rwx bits) is the authorizer's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Read file data, or list a directory's contents.
    Read,
    /// Search a directory for a name.
    Search,
    /// Modify a file's contents or attributes.
    Modify,
    /// Create, remove or rename an entry of a directory.
    ModifyDir,
}

/// The authorization interface the filesystem depends on.
///
/// Implementations own the mapping from [`Access`] intents (and the sticky
/// and ACCESS3 procedures) to their policy. MirrorFS holds one of these and
/// never dispatches on policy-specific details itself.
pub trait Authorizer: std::fmt::Debug + Send + Sync {
    /// Resolve a raw caller [`Credential`] into a flat [`Credentials`] identity.
    fn map_credentials(&self, cred: &Credential) -> Credentials;

    /// Does the caller hold `access` on the object described by `attr`?
    fn allows(&self, cred: &Credentials, attr: &file::Attr, access: Access) -> bool;

    /// Sticky-bit ownership check on a directory (`S_ISVTX` semantics): is the
    /// caller allowed to unlink/rename a victim owned by `victim_uid` out of the
    /// `parent` directory?
    fn sticky_allows(&self, cred: &Credentials, parent: &file::Attr, victim_uid: u32) -> bool;

    /// The advisory `ACCESS3` mask granted on `attr` among `requested`.
    fn access3(
        &self,
        cred: &Credentials,
        attr: &file::Attr,
        requested: access::Mask,
    ) -> access::Mask;
}
