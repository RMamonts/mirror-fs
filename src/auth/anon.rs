//! Anonymous (`AUTH_NONE`) authorization.
//!
//! A caller with no verified identity always falls into the weakest POSIX class
//! ("other") and is never treated as root.

use nfs_mamont::vfs::access;
use nfs_mamont::vfs::file;

use super::policy::Policy;
use super::posix::{grant, Class};

/// Authorization for anonymous callers.
pub struct AnonPolicy;

impl Policy for AnonPolicy {
    fn grant(&self, attr: &file::Attr, requested: access::Mask) -> access::Mask {
        grant(Class::Other, false, attr, requested)
    }
}