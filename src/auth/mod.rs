//! Authorization: map a [`Credential`] onto a [`Policy`] and ask for a decision.
//!
//! The [`Credential`] enum is *only* matched here. The variant is resolved up
//! front, before any permission computation, so the POSIX permission model is
//! just one policy behind [`Authorization`] and non-POSIX credentials can be
//! added without touching that model.

pub mod anon;
pub mod policy;
pub mod posix;

use nfs_mamont::auth::{AuthSysParams, Credential};
use nfs_mamont::vfs::access;
use nfs_mamont::vfs::file;

use policy::Policy;
use anon::AnonPolicy;
use posix::PosixPolicy;

/// A resolved caller identity, ready to be authorized.
///
/// This is the seam where the set of supported credentials is extended: add a
/// variant here, teach [`Authorization::from_credential`] to map a new
/// [`Credential`] onto it, and provide an authorizer in [`Authorization::policy`].
pub enum Authorization {
    /// `AUTH_NONE`: anonymous caller, weakest rights.
    Anonymous,
    /// `AUTH_SYS`: resolved UNIX-style identity.
    Posix(AuthSysParams),
}

impl Authorization {
    /// Resolves the caller's credentials into an authorizable identity.
    ///
    /// The credential type is determined here, *before* any mask computation.
    pub fn from_credential(cred: Credential) -> Authorization {
        match cred {
            Credential::None => Authorization::Anonymous,
            Credential::Sys(params) => Authorization::Posix(params),
        }
    }

    /// Returns the policy that decides what this identity may do.
    pub fn policy(&self) -> Box<dyn Policy> {
        match self {
            Authorization::Anonymous => Box::new(AnonPolicy),
            Authorization::Posix(params) => Box::new(PosixPolicy::new(params.clone())),
        }
    }

    /// The NFS rights this identity is granted on `attr` among `requested`.
    pub fn grant(&self, attr: &file::Attr, requested: access::Mask) -> access::Mask {
        self.policy().grant(attr, requested)
    }

    /// True if every right in `required` is granted on `attr`.
    pub fn allowed(&self, attr: &file::Attr, required: access::Mask) -> bool {
        self.policy().allowed(attr, required)
    }
}