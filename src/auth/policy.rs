use nfs_mamont::vfs::access;
use nfs_mamont::vfs::file;

/// An authorization policy computes what an identity may do on an object.
///
/// This is the decision contract, deliberately independent of *how* a caller is
/// identified. Each concrete policy owns its own permission model and its own
/// translation onto NFS [`access::Mask`] rights — nothing here assumes POSIX.
pub trait Policy {
    /// The NFS rights `this` identity is granted on `attr` among `requested`.
    fn grant(&self, attr: &file::Attr, requested: access::Mask) -> access::Mask;

    /// True if every bit in `required` is granted on `attr`.
    ///
    /// Implementations may override for cheap per-flavor short-circuits, but the
    /// default derives the answer from [`Policy::grant`].
    fn allowed(&self, attr: &file::Attr, required: access::Mask) -> bool {
        let granted = self.grant(attr, access::Mask::from_wire(required.bits()));
        (granted.bits() & required.bits()) == required.bits()
    }
}