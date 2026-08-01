//! Running the indexers without their toolchains being on the machine.
//!
//! Docker is cairn's one dependency. Indexing needs `scip-go` and `scip-python`, which in
//! turn need a Go toolchain and Node; asking someone to install four things before a tool
//! does anything is how a tool goes uninstalled. So the toolchains live in an image and
//! the host stays clean.
//!
//! Nothing is pulled. The Dockerfile is embedded in the binary and built locally, so there
//! is no registry to reach, nothing to authenticate against, and no way for the image to
//! disagree with the cairn that drives it.
//!
//! **The image is shared by every repository on the machine**, the same way one binary and
//! one settings file serve them all. It is tagged with cairn's version, so it is built once
//! per cairn release and then reused — the second repository you index pays nothing.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DOCKERFILE: &str = include_str!("../assets/indexers.Dockerfile");

/// The shared image's name. Version-tagged so upgrading cairn rebuilds it rather than
/// silently reusing indexers from an older release.
pub fn image_tag() -> String {
    format!("cairn-indexers:{}", env!("CARGO_PKG_VERSION"))
}

/// Is there a working Docker to talk to?
///
/// `docker version` rather than `--version`: the second answers from the client alone and
/// succeeds when the daemon is not running, which is exactly the case worth catching.
pub fn available() -> bool {
    Command::new("docker")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// What `ensure_image` had to do, so the caller can say so only when it is worth saying.
pub enum Image {
    AlreadyBuilt,
    Built,
}

/// Build the shared image if this machine has not got it yet.
pub fn ensure_image() -> Result<Image> {
    let tag = image_tag();
    let present = Command::new("docker")
        .args(["image", "inspect", &tag])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if present {
        return Ok(Image::AlreadyBuilt);
    }

    let context = build_context()?;
    // Output is inherited rather than captured: this is the slow step, and a progress
    // display someone can watch beats several silent minutes.
    let status = Command::new("docker")
        .arg("build")
        .arg("--tag")
        .arg(&tag)
        .arg(&context)
        .status()
        .context("running `docker build`")?;
    if !status.success() {
        bail!("could not build the indexer image ({tag})");
    }
    Ok(Image::Built)
}

/// Where the embedded Dockerfile is written to be built from.
///
/// Per user, not per repository: the image is machine-wide and shared by every checkout,
/// so build files belong nowhere near one of them.
///
/// Deliberately not beside the binary. That is where settings live, but a binary can sit
/// somewhere its user cannot write — `/usr/local/bin` installed by root and run by
/// somebody else is the ordinary case — and a build context that fails there would make
/// indexing depend on how cairn happened to be installed.
fn build_context() -> Result<PathBuf> {
    let home = std::env::var_os("CAIRN_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cairn")))
        .or_else(|| std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join(".cairn")));

    let dir = match home {
        Some(h) => h.join("indexers"),
        None => std::env::temp_dir().join("cairn-indexers"),
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("Dockerfile");
    // Rewritten every time rather than only when absent: an edited or truncated file here
    // would build an image that is not the one this cairn was published with.
    std::fs::write(&path, DOCKERFILE).with_context(|| format!("writing {}", path.display()))?;
    Ok(dir)
}

/// Run one command in the image, with `repo` mounted and `workdir` as the directory to run
/// it in (relative to the repository root).
pub fn run(repo: &Path, workdir: &Path, args: &[&str]) -> Result<std::process::Output> {
    let repo =
        std::fs::canonicalize(repo).with_context(|| format!("resolving {}", repo.display()))?;
    // The container sees the repository at a fixed path, so the paths recorded in the SCIP
    // output do not depend on where the repository happens to live on this machine.
    let rel = workdir.strip_prefix(&repo).unwrap_or(Path::new(""));
    let container_workdir = Path::new("/repo").join(rel);

    let mut cmd = Command::new("docker");
    cmd.arg("run")
        .arg("--rm")
        .arg("--volume")
        .arg(format!("{}:/repo", repo.display()))
        .arg("--workdir")
        .arg(&container_workdir);

    // Run as the caller, so the .scip files it writes belong to them rather than to root
    // and a later `cairn index` without sudo can replace them.
    #[cfg(unix)]
    {
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        cmd.arg("--user").arg(format!("{uid}:{gid}"));
    }

    cmd.arg(image_tag())
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("running `docker run`")
}

/// What to tell someone who has not got Docker.
pub const NO_DOCKER: &str = "\
cairn needs Docker to index, and cannot reach one.

The language indexers need a Go toolchain and Node; rather than asking you to install
those, cairn keeps them in an image it builds itself. Docker is the only thing it needs
from you — install it, start it, and run `cairn index` again.

  https://docs.docker.com/get-started/get-docker/";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tag_follows_cairns_own_version() {
        // The whole point of the tag: a new cairn must not reuse an old image.
        assert_eq!(
            image_tag(),
            format!("cairn-indexers:{}", env!("CARGO_PKG_VERSION"))
        );
        assert!(image_tag().starts_with("cairn-indexers:"));
    }

    #[test]
    fn the_embedded_dockerfile_is_the_real_one() {
        assert!(DOCKERFILE.contains("scip-go"), "the Go indexer is missing");
        assert!(
            DOCKERFILE.contains("scip-python"),
            "the Python indexer is missing"
        );
        assert!(
            DOCKERFILE.starts_with('#'),
            "expected the explanatory header"
        );
    }
}
