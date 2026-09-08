use nfs_mamont::auth::{AuthSysParams, Credential};
use nfs_mamont::vfs::access;
use nfs_mamont::vfs::file;

use super::MirrorFS;

/// The permission class a caller belongs to with respect to an object's owner
/// and group. It selects which of the three `rwx` triples in `mode` applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
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

// ---------------------------------------------------------------------------
// Layer 1 — enum `Credential` level: classify the caller, per variant.
// ---------------------------------------------------------------------------

/// Which triple of `mode` bits applies to `cred` on `attr`.
///
/// This is the enum-level dispatch: each [`Credential`] variant is handed off
/// to its own authorization handler below. The access logic never pattern
/// matches on `Credential` again.
fn class_of(cred: &Credential, attr: &file::Attr) -> Class {
    match cred {
        // AUTH_NONE: no verified identity — only the "other" triple may apply.
        Credential::None => none_class(),
        // AUTH_SYS: full POSIX classification.
        Credential::Sys(params) => sys_class(params, attr),
    }
}

/// Whether `cred` is treated as root (`uid == 0`).
fn is_root(cred: &Credential) -> bool {
    matches!(cred, Credential::Sys(AuthSysParams { uid: 0, .. }))
}

// ---------------------------------------------------------------------------
// Layer 2 — per-variant authorization handlers (full POSIX).
// ---------------------------------------------------------------------------

/// (AUTH_SYS) POSIX classification: owner by `uid`, then primary `gid`, then
/// any supplementary `gid`, otherwise "other".
fn sys_class(params: &AuthSysParams, attr: &file::Attr) -> Class {
    if params.uid == attr.uid {
        Class::Owner
    } else if params.gid == attr.gid || params.gids.contains(&attr.gid) {
        Class::Group
    } else {
        Class::Other
    }
}

/// (AUTH_NONE) Anonymous callers always land in the "other" triple.
fn none_class() -> Class {
    Class::Other
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

/// Step 4 — reverse translation: the `rwx` primitive(s) a single NFS right
/// demands from the `mode` check in order to be granted. Used only to frame
/// the [`access::Access`] decision in rwx terms.
fn nfs_to_rwx(right: u32) -> Rwx {
    match right {
        access::Mask::READ => Rwx::new(true, false, false),
        // MODIFY / EXTEND / DELETE all reduce to the write bit.
        access::Mask::MODIFY | access::Mask::EXTEND => {
            Rwx::new(false, true, false)
        }
        // EXECUTE and LOOKUP (search) need the execute bit.
        access::Mask::EXECUTE | access::Mask::LOOKUP | access::Mask::DELETE => Rwx::new(false, false, true),
        _ => Rwx::default(),
    }
}

/// Step 3 — forward translation: the NFS right(s) that follow from holding an
/// `rwx` permission. Used to render the granted [`Rwx`] back into a black mask.
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

impl access::Access for MirrorFS {
    async fn access(&self, cred: Credential, args: access::Args) -> Result<access::Success, access::Fail> {
        let path = match self.path_for_handle(&args.file).await {
            Ok(path) => path,
            Err(error) => return Err(access::Fail { error, object_attr: None }),
        };
        let meta = match Self::metadata(&path) {
            Ok(meta) => meta,
            Err(error) => return Err(access::Fail { error, object_attr: None }),
        };
        let attr = Self::attr_from_metadata(&meta);
        let granted = Self::compute_access_mask(&attr, &cred, args.mask);
        Ok(access::Success { object_attr: Some(attr), access: granted })
    }
}

impl MirrorFS {
    /// Computes the granted [`access::Mask`] for `cred` against `attr`'s mode.
    ///
    /// The whole decision is performed in rwx terms:
    /// 1. the caller is classified (owner/group/other) from its [`Credential`],
    /// 2. [`posix_perms`] grants an [`Rwx`] from `mode`,
    /// 3. that [`Rwx`] is translated back into an [`access::Mask`],
    /// 4. each requested NFS right is affirmed by checking the `rwx` its
    ///    [`nfs_to_rwx`] demands against the granted [`Rwx`].
    pub(super) fn compute_access_mask(
        attr: &file::Attr,
        cred: &Credential,
        requested: access::Mask,
    ) -> access::Mask {
        // (1) which triple applies, (2) the posix rwx decision.
        let granted = posix_perms(class_of(cred, attr), is_root(cred), attr.mode);
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
}