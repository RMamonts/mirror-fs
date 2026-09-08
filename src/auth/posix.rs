//! POSIX-style authorization for `AUTH_SYS` credentials.
//!
//! The whole rwx model — class classification (owner/group/other), the
//! read/write/execute decision, and the translation onto NFS [`access::Mask`]
//! bits — lives here, owned by [`PosixPolicy`]. It never sees a
//! [`Credential`]; it is constructed from the already-picked [`AuthSysParams`].

use nfs_mamont::auth::AuthSysParams;
use nfs_mamont::vfs::access;
use nfs_mamont::vfs::file;

use super::policy::Policy;

/// The permission class a caller belongs to with respect to an object's owner
/// and group. It selects which of the three `rwx` triples in `mode` applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    Owner,
    Group,
    Other,
}

impl Class {
    /// Bit offset within `mode` for this class: owner is `0o700`, group `0o070`,
    /// other `0o007`.
    fn shift(self) -> u32 {
        match self {
            Class::Owner => 6,
            Class::Group => 3,
            Class::Other => 0,
        }
    }
}

/// A POSIX `rwx` access decision.
///
/// This is the *single* representation the authorization check actually
/// operates on — never the raw [`access::Mask`]. NFS flags are only translated
/// into [`Rwx`] to ask "may they?", and back into [`access::Mask`] to answer.
#[derive(Clone, Copy, Debug, Default)]
struct Rwx {
    read: bool,
    write: bool,
    execute: bool,
}

impl Rwx {
    fn new(read: bool, write: bool, execute: bool) -> Self {
        Self { read, write, execute }
    }

    /// Whether this set grants every primitive that `need` requires.
    fn satisfies(&self, need: &Rwx) -> bool {
        (!need.read || self.read) && (!need.write || self.write) && (!need.execute || self.execute)
    }
}

/// POSIX classification: owner by `uid`, then primary `gid`, then any
/// supplementary `gid`, otherwise "other".
fn sys_class(params: &AuthSysParams, attr: &file::Attr) -> Class {
    if params.uid == attr.uid {
        Class::Owner
    } else if params.gid == attr.gid || params.gids.contains(&attr.gid) {
        Class::Group
    } else {
        Class::Other
    }
}

/// POSIX authorization — step 2. Given the caller's [`Class`] (hence which
/// triple of `mode` to use) and whether it is root, grants the [`Rwx`] the
/// caller effectively has.
///
/// Root may read and write anything and traverse everywhere, but still needs at
/// least one execute bit somewhere in `mode` before it may run or search —
/// mirroring the kernel.
fn posix_perms(class: Class, is_root: bool, mode: u32) -> Rwx {
    let shift = class.shift();
    let r = (mode >> shift) & 0o4 != 0;
    let w = (mode >> shift) & 0o2 != 0;
    let any_x = mode & 0o111 != 0;
    Rwx::new(
        r || is_root,
        w || is_root,
        ((mode >> shift) & 0o1 != 0) || (is_root && any_x),
    )
}

// ---------------------------------------------------------------------------
// NFS <-> rwx translation (both directions).
// ---------------------------------------------------------------------------

const ALL_RIGHTS: [u32; 6] = [
    access::Mask::READ,
    access::Mask::LOOKUP,
    access::Mask::MODIFY,
    access::Mask::EXTEND,
    access::Mask::DELETE,
    access::Mask::EXECUTE,
];

/// The `rwx` primitive(s) a single NFS right demands in order to be granted.
/// Used to frame the decision in rwx terms.
fn nfs_to_rwx(right: u32) -> Rwx {
    match right {
        access::Mask::READ => Rwx::new(true, false, false),
        // MODIFY / EXTEND both reduce to the write bit.
        access::Mask::MODIFY | access::Mask::EXTEND => Rwx::new(false, true, false),
        // EXECUTE, LOOKUP (search) and DELETE map onto the execute bit.
        access::Mask::EXECUTE | access::Mask::LOOKUP | access::Mask::DELETE => {
            Rwx::new(false, false, true)
        }
        _ => Rwx::default(),
    }
}

/// The NFS right(s) that follow from holding an `rwx` permission.
fn rwx_to_nfs(rights: &Rwx) -> u32 {
    let mut flags = 0;
    if rights.read {
        flags |= access::Mask::READ;
    }
    if rights.write {
        flags |= access::Mask::MODIFY | access::Mask::EXTEND;
    }
    if rights.execute {
        flags |= access::Mask::EXECUTE | access::Mask::DELETE;
    }
    flags
}

/// The shared rwx decision and NFS translation, parameterized only by the
/// caller's [`Class`] and whether it is root. Used by both [`PosixPolicy`] and
/// the anonymous policy.
pub(crate) fn grant(class: Class, is_root: bool, attr: &file::Attr, requested: access::Mask) -> access::Mask {
    // (1) which triple applies, (2) the posix rwx decision.
    let granted = posix_perms(class, is_root, attr.mode);
    let is_dir = matches!(attr.file_type, file::Type::Directory);

    // (3) the NFS rights that follow from the granted rwx.
    let mut result = rwx_to_nfs(&granted) & requested.bits();

    // LOOKUP (search) is a directory-only right.
    if !is_dir {
        result &= !access::Mask::LOOKUP;
    }

    // (4) affirm each surviving right against the rwx it demands, so the
    // decision is verifiably rwx-based in both translation directions.
    for right in ALL_RIGHTS {
        if (result & right) != 0 && !granted.satisfies(&nfs_to_rwx(right)) {
            result &= !right;
        }
    }

    access::Mask::from_wire(result)
}

/// POSIX authorization for a resolved `AUTH_SYS` identity.
pub struct PosixPolicy {
    params: AuthSysParams,
}

impl PosixPolicy {
    pub fn new(params: AuthSysParams) -> Self {
        Self { params }
    }

    fn is_root(&self) -> bool {
        self.params.uid == 0
    }

    fn class_of(&self, attr: &file::Attr) -> Class {
        sys_class(&self.params, attr)
    }
}

impl Policy for PosixPolicy {
    fn grant(&self, attr: &file::Attr, requested: access::Mask) -> access::Mask {
        grant(self.class_of(attr), self.is_root(), attr, requested)
    }
}