//! POSIX/Linux permission rules that mirror-fs must enforce itself: it performs
//! every operation as the server user, so the kernel cannot apply them on the
//! NFS caller's behalf. The tests run both unprivileged (as in CI) and as root.

use std::fs as stdfs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use nfs_mamont::vfs;
use nfs_mamont::vfs::{rename, set_attr, write};

use crate::auth;
use crate::fs::MirrorFS;

use super::helpers::{
    create_dir, cred_for, default_new_attr, dir_op, expect_err, expect_ok, slice_from_bytes,
    unprivileged_owner, write_file, TestContext,
};

fn chown_attr(uid: u32, gid: u32) -> set_attr::NewAttr {
    set_attr::NewAttr { uid: Some(uid), gid: Some(gid), ..default_new_attr() }
}

fn mode_attr(mode: u32) -> set_attr::NewAttr {
    set_attr::NewAttr { mode: Some(mode), ..default_new_attr() }
}

fn mode_of(path: &std::path::Path) -> u32 {
    stdfs::symlink_metadata(path).unwrap().mode() & 0o7777
}

#[tokio::test]
async fn chown_by_non_owner_is_rejected_even_without_change() {
    let ctx = TestContext::new();
    let path = write_file(ctx.root_path(), "file", b"x");
    let (uid, gid) = unprivileged_owner(&path);
    let root = ctx.root_handle().await;
    let handle = ctx.lookup_handle(root, "file").await;

    // Linux chown_ok()/chgrp_ok(): naming an owner or group at all is reserved
    // to the owner (or a privileged caller), even when nothing would change.
    let stranger = cred_for(uid + 1, gid + 1, &[]);
    let err = expect_err(
        set_attr::SetAttr::set_attr(
            &ctx.fs,
            set_attr::Args { file: handle.clone(), new_attr: chown_attr(uid, gid), guard: None },
            &stranger,
        )
        .await,
        "a non-owner must not chown, even to the current owner",
    );
    assert_eq!(err.error, vfs::Error::Permission);

    let owner = cred_for(uid, gid, &[]);
    expect_ok(
        set_attr::SetAttr::set_attr(
            &ctx.fs,
            set_attr::Args { file: handle, new_attr: chown_attr(uid, gid), guard: None },
            &owner,
        )
        .await,
        "the owner may set its own uid and current gid",
    );
}

#[tokio::test]
async fn chmod_by_owner_outside_the_group_drops_setgid() {
    let ctx = TestContext::new();
    let path = write_file(ctx.root_path(), "file", b"x");
    let (uid, gid) = unprivileged_owner(&path);
    let root = ctx.root_handle().await;
    let handle = ctx.lookup_handle(root, "file").await;

    let outsider = cred_for(uid, gid + 1, &[]);
    expect_ok(
        set_attr::SetAttr::set_attr(
            &ctx.fs,
            set_attr::Args { file: handle.clone(), new_attr: mode_attr(0o2755), guard: None },
            &outsider,
        )
        .await,
        "the owner may chmod its file",
    );
    assert_eq!(mode_of(&path), 0o755, "S_ISGID must be dropped for a non-member");

    let member = cred_for(uid, gid, &[]);
    expect_ok(
        set_attr::SetAttr::set_attr(
            &ctx.fs,
            set_attr::Args { file: handle, new_attr: mode_attr(0o2755), guard: None },
            &member,
        )
        .await,
        "the owner may chmod its file",
    );
    assert_eq!(mode_of(&path), 0o2755, "a member of the group keeps S_ISGID");
}

#[tokio::test]
async fn write_by_unprivileged_caller_clears_setuid_and_setgid() {
    let ctx = TestContext::new();
    let path = write_file(ctx.root_path(), "file", b"");
    let (uid, gid) = unprivileged_owner(&path);
    stdfs::set_permissions(&path, stdfs::Permissions::from_mode(0o6777)).unwrap();
    assert_eq!(mode_of(&path), 0o6777, "test setup should set S_ISUID and S_ISGID");
    let root = ctx.root_handle().await;
    let handle = ctx.lookup_handle(root, "file").await;

    // A privileged caller keeps the bits (it has CAP_FSETID). This only holds
    // when the server itself runs as root: an unprivileged server process gets
    // the bits stripped by the kernel on its own write regardless.
    let privileged = cred_for(0, 0, &[]);
    let args = write::Args {
        file: handle.clone(),
        offset: 0,
        size: 1,
        stable: write::StableHow::FileSync,
        data: slice_from_bytes(b"a").await,
    };
    expect_ok(write::Write::write(&ctx.fs, args, &privileged).await, "root write should succeed");
    // SAFETY: geteuid(2) has no preconditions and cannot fail.
    if unsafe { libc::geteuid() } == 0 {
        assert_eq!(mode_of(&path), 0o6777);
    } else {
        stdfs::set_permissions(&path, stdfs::Permissions::from_mode(0o6777)).unwrap();
    }

    // Anyone else drops them before the data is written (file_remove_privs()).
    let writer = cred_for(uid + 1, gid + 1, &[]);
    let args = write::Args {
        file: handle,
        offset: 1,
        size: 1,
        stable: write::StableHow::FileSync,
        data: slice_from_bytes(b"b").await,
    };
    expect_ok(write::Write::write(&ctx.fs, args, &writer).await, "write should succeed");
    assert_eq!(mode_of(&path), 0o777);
    assert_eq!(stdfs::read(&path).unwrap(), b"ab");
}

#[tokio::test]
async fn moving_a_directory_to_a_new_parent_needs_write_on_it() {
    let ctx = TestContext::new();
    let a = create_dir(ctx.root_path(), "a");
    let b = create_dir(ctx.root_path(), "b");
    let sub = create_dir(ctx.root_path(), "a/sub");
    for dir in [&a, &b] {
        stdfs::set_permissions(dir, stdfs::Permissions::from_mode(0o777)).unwrap();
    }
    stdfs::set_permissions(&sub, stdfs::Permissions::from_mode(0o755)).unwrap();
    let (uid, gid) = unprivileged_owner(&sub);
    let root = ctx.root_handle().await;
    let a_handle = ctx.lookup_handle(root.clone(), "a").await;
    let b_handle = ctx.lookup_handle(root, "b").await;

    // May modify both parents, but cannot rewrite `sub`'s `..` entry.
    let stranger = cred_for(uid + 1, gid + 1, &[]);
    let err = expect_err(
        rename::Rename::rename(
            &ctx.fs,
            rename::Args { from: dir_op(a_handle.clone(), "sub"), to: dir_op(b_handle, "sub") },
            &stranger,
        )
        .await,
        "moving a directory to a new parent needs write permission on it",
    );
    assert_eq!(err.error, vfs::Error::Access);

    // Renaming within the same parent does not touch `..`.
    expect_ok(
        rename::Rename::rename(
            &ctx.fs,
            rename::Args { from: dir_op(a_handle.clone(), "sub"), to: dir_op(a_handle, "sub2") },
            &stranger,
        )
        .await,
        "a same-parent rename only needs write permission on the parent",
    );
    assert!(a.join("sub2").is_dir());
}

#[test]
fn new_objects_inherit_the_group_of_a_setgid_parent() {
    let dir = tempfile::tempdir().unwrap();
    let plain = stdfs::metadata(dir.path()).unwrap();
    let caller = auth::Credentials {
        uid: 4242,
        gid: plain.gid() + 1,
        groups: Vec::new(),
        privileged: false,
    };
    assert_eq!(MirrorFS::new_object_owner(&caller, &plain), (4242, caller.gid));

    stdfs::set_permissions(dir.path(), stdfs::Permissions::from_mode(0o2775)).unwrap();
    let setgid = stdfs::metadata(dir.path()).unwrap();
    assert_ne!(setgid.mode() & 0o2000, 0, "test setup should set S_ISGID");
    assert_eq!(MirrorFS::new_object_owner(&caller, &setgid), (4242, setgid.gid()));
}

#[tokio::test]
async fn owner_keeps_data_access_to_a_file_it_made_read_only() {
    let ctx = TestContext::new();
    let path = write_file(ctx.root_path(), "file", b"x");
    let (uid, gid) = unprivileged_owner(&path);
    stdfs::set_permissions(&path, stdfs::Permissions::from_mode(0o444)).unwrap();
    let root = ctx.root_handle().await;
    let handle = ctx.lookup_handle(root, "file").await;

    // knfsd's NFSD_MAY_OWNER_OVERRIDE: e.g. open(O_CREAT|O_WRONLY, 0444) + write().
    let owner = cred_for(uid, gid, &[]);
    expect_ok(
        ctx.fs.require_data_access(&owner, &handle, auth::Access::Modify).await,
        "the owner keeps write access to its read-only file",
    );
    let stranger = cred_for(uid + 1, gid + 1, &[]);
    let err = expect_err(
        ctx.fs.require_data_access(&stranger, &handle, auth::Access::Modify).await,
        "others still need the write bit",
    );
    assert_eq!(err, vfs::Error::Access);
}
