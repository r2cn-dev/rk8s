use std::fs;
use std::path::{Path, PathBuf};
use anyhow::{bail, Result};
use libcontainer::utils::{create_dir_all_with_mode, rootless_required};
use nix::libc;
use nix::sys::stat::Mode;
use nix::unistd::getuid;

pub fn determine(root_path: Option<PathBuf>) -> Result<PathBuf> {
    let uid = getuid().as_raw();

    if let Some(path) = root_path {
        if !path.exists() {
            create_dir_all_with_mode(&path, uid, Mode::S_IRWXU)?;
        }
        let path = path.canonicalize()?;
        return Ok(path);
    }

    if !rootless_required()? {
        let path = get_default_not_rootless_path();
        create_dir_all_with_mode(&path, uid, Mode::S_IRWXU)?;
        return Ok(path);
    }

    // see https://specifications.freedesktop.org/basedir-spec/basedir-spec-latest.html
    if let Ok(path) = std::env::var("XDG_RUNTIME_DIR") {
        let path = Path::new(&path).join("youki");
        if create_dir_all_with_mode(&path, uid, Mode::S_IRWXU).is_ok() {
            return Ok(path);
        }
    }

    // XDG_RUNTIME_DIR is not set, try the usual location
    let path = get_default_rootless_path(uid);
    if create_dir_all_with_mode(&path, uid, Mode::S_IRWXU).is_ok() {
        return Ok(path);
    }

    if let Ok(path) = std::env::var("HOME") {
        if let Ok(resolved) = fs::canonicalize(path) {
            let run_dir = resolved.join(".youki/run");
            if create_dir_all_with_mode(&run_dir, uid, Mode::S_IRWXU).is_ok() {
                return Ok(run_dir);
            }
        }
    }

    let tmp_dir = PathBuf::from(format!("/tmp/youki-{uid}"));
    if create_dir_all_with_mode(&tmp_dir, uid, Mode::S_IRWXU).is_ok() {
        return Ok(tmp_dir);
    }

    bail!("could not find a storage location with suitable permissions for the current user");
}

#[cfg(not(test))]
fn get_default_not_rootless_path() -> PathBuf {
    PathBuf::from("/run/youki")
}

#[cfg(test)]
fn get_default_not_rootless_path() -> PathBuf {
    std::env::temp_dir().join("default_youki_path")
}

#[cfg(not(test))]
fn get_default_rootless_path(uid: libc::uid_t) -> PathBuf {
    PathBuf::from(format!("/run/user/{uid}/youki"))
}

#[cfg(test)]
fn get_default_rootless_path(uid: libc::uid_t) -> PathBuf {
    std::env::temp_dir().join(format!("default_rootless_youki_path_{uid}").as_str())
}

