//! POSIX user/group/other (rwx + sticky bit) authorization.
//!
//! [`PosixAuthorizer`] implements the [`Authorizer`] interface using the
//! classic POSIX dac model: pick one class (owner / group / other) by the
//! caller's identity, compare its rwx triple, apply the sticky bit where needed,
//! and fold `UID 0` (unless squashed) into a special privileged path.

use nfs_mamont::auth::Credential;
use nfs_mamont::vfs::access;
use nfs_mamont::vfs::file;

use super::{Access, Authorizer, Credentials};

/// Default anonymous uid/gid, used for `AUTH_NONE` and for squashed root.
const ANON_UID: u32 = 65534;
const ANON_GID: u32 = 65534;

/// rwx primitives, encoded as the low three permission bits.
const MODE_READ: u32 = 0b100;
const MODE_WRITE: u32 = 0b010;
const MODE_EXEC: u32 = 0b001;

/// POSIX authorization policy over rwx mode bits and the sticky bit.
#[derive(Debug)]
pub struct PosixAuthorizer {
    /// When `true`, a remote `UID 0` is squashed to the anonymous identity and
    /// gains no privileges.
    root_squash: bool,
}

impl PosixAuthorizer {
    /// Creates a policy with the given `root_squash` option.
    pub fn new(root_squash: bool) -> Self {
        Self { root_squash }
    }
}

impl Authorizer for PosixAuthorizer {
    fn map_credentials(&self, cred: &Credential) -> Credentials {
        resolve_credential(cred.clone(), self.root_squash)
    }

    fn allows(&self, cred: &Credentials, attr: &file::Attr, access: Access) -> bool {
        let required = match access {
            Access::Read => MODE_READ,
            Access::Search => MODE_EXEC,
            Access::Modify => MODE_WRITE,
            Access::ModifyDir => MODE_WRITE | MODE_EXEC,
        };
        mode_allows(cred, attr, required)
    }

    fn sticky_allows(&self, cred: &Credentials, parent: &file::Attr, victim_uid: u32) -> bool {
        sticky_allows(cred, parent, victim_uid)
    }

    fn access3(&self, cred: &Credentials, attr: &file::Attr, requested: access::Mask) -> access::Mask {
        compute_access3(cred, attr, requested)
    }
}

/// Resolves a raw credential into a flat identity.
///
/// `root_squash` is consulted *before* `privileged` is derived: a squashed
/// UID 0 collapses to the anonymous identity and gains no privileges.
fn resolve_credential(cred: Credential, root_squash: bool) -> Credentials {
    let (uid, gid, groups) = match cred {
        Credential::None => (ANON_UID, ANON_GID, Vec::new()),
        Credential::Sys(params) => (params.uid, params.gid, dedup_groups(&params.gids)),
    };

    if root_squash && uid == 0 {
        return Credentials { uid: ANON_UID, gid: ANON_GID, groups: Vec::new(), privileged: false };
    }

    Credentials { uid, gid, groups, privileged: uid == 0 }
}

/// The rwx triple granted by the caller's class on `attr`. Exactly one class
/// (owner / group / other) is selected; class bits never merge.
fn select_class_bits(cred: &Credentials, attr: &file::Attr) -> u32 {
    if cred.uid == attr.uid {
        (attr.mode >> 6) & 0b111
    } else if cred.gid == attr.gid || cred.groups.contains(&attr.gid) {
        (attr.mode >> 3) & 0b111
    } else {
        attr.mode & 0b111
    }
}

/// Does the caller hold every primitive in `required`?
///
/// Root goes down a separate path rather than being folded into the bit
/// comparison.
fn mode_allows(cred: &Credentials, attr: &file::Attr, required: u32) -> bool {
    if cred.privileged {
        return privileged_mode_allows(attr, required);
    }
    (select_class_bits(cred, attr) & required) == required
}

/// Privileged (un-squashed) root may act as any class, so it effectively holds
/// the *union* of the owner, group and other bits: a right still grants nothing
/// if no class sets that bit anywhere. Directory `X` (traversal) is always
/// allowed; executing a regular file still requires a real execute bit.
fn privileged_mode_allows(attr: &file::Attr, required: u32) -> bool {
    if (required & MODE_EXEC) != 0 && matches!(attr.file_type, file::Type::Directory) {
        return true;
    }
    let union = (attr.mode >> 6) | (attr.mode >> 3) | attr.mode;
    (union & required) == required
}

/// Sticky-bit (`S_ISVTX`) ownership check for unlink/rmdir/rename.
///
/// Allowed when the sticky bit is unset, the caller is privileged, owns the
/// parent directory, or owns the victim.
fn sticky_allows(cred: &Credentials, parent: &file::Attr, victim_uid: u32) -> bool {
    (parent.mode & 0o1000) == 0
        || cred.privileged
        || cred.uid == parent.uid
        || cred.uid == victim_uid
}

/// The advisory `ACCESS3` mask granted on `attr` among `requested`.
///
/// Each ACCESS3 bit is decided by the *set* of rwx primitives it needs for the
/// given object type; there is intentionally no reverse "NFS bit -> single rwx
/// primitive" map.
fn compute_access3(cred: &Credentials, attr: &file::Attr, requested: access::Mask) -> access::Mask {
    const READ: u32 = access::Mask::READ;
    const LOOKUP: u32 = access::Mask::LOOKUP;
    const MODIFY: u32 = access::Mask::MODIFY;
    const EXTEND: u32 = access::Mask::EXTEND;
    const DELETE: u32 = access::Mask::DELETE;
    const EXECUTE: u32 = access::Mask::EXECUTE;

    let mut allowed = 0u32;

    match attr.file_type {
        file::Type::Directory => {
            if mode_allows(cred, attr, MODE_READ) {
                allowed |= READ;
            }
            if mode_allows(cred, attr, MODE_EXEC) {
                allowed |= LOOKUP;
            }
            if mode_allows(cred, attr, MODE_WRITE | MODE_EXEC) {
                allowed |= MODIFY | EXTEND | DELETE;
            }
        }
        file::Type::Regular => {
            if mode_allows(cred, attr, MODE_READ) {
                allowed |= READ;
            }
            if mode_allows(cred, attr, MODE_WRITE) {
                allowed |= MODIFY | EXTEND;
            }
            if mode_allows(cred, attr, MODE_EXEC) {
                allowed |= EXECUTE;
            }
        }
        file::Type::Symlink => {
            // Symlink permissions do not take part in POSIX DAC.
        }
        _ => {
            // FIFO/socket/device follow the regular-file read/write parts.
            if mode_allows(cred, attr, MODE_READ) {
                allowed |= READ;
            }
            if mode_allows(cred, attr, MODE_WRITE) {
                allowed |= MODIFY | EXTEND;
            }
        }
    }

    access::Mask::from_wire(allowed & requested.bits())
}

fn dedup_groups(gids: &[u32]) -> Vec<u32> {
    let mut seen = Vec::new();
    for gid in gids {
        if !seen.contains(gid) {
            seen.push(*gid);
        }
    }
    seen
}