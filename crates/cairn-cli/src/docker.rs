//! Running the indexers without their toolchains being on the machine.
//!
//! Docker is cairn's one dependency. Indexing needs `scip-go`, `scip-python` and
//! `scip-typescript`, which in turn need a Go toolchain and Node; asking someone to
//! install four things before a tool does anything is how a tool goes uninstalled. So the
//! toolchains live in an image and the host stays clean.
//!
//! One thing the image cannot supply: a TypeScript project's own `node_modules`. Those
//! belong to the repository, and without them scip-typescript resolves nothing at all.
//!
//! Nothing is pulled. The Dockerfile is embedded in the binary and built locally, so there
//! is no registry to reach, nothing to authenticate against, and no way for the image to
//! disagree with the cairn that drives it.
//!
//! **The image is shared by every repository on the machine**, the same way one binary and
//! one settings file serve them all. It is tagged with cairn's version, so it is built once
//! per cairn release and then reused — the second repository you index pays nothing.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

const DOCKERFILE: &str = include_str!("../assets/indexers.Dockerfile");

/// The flow provider, embedded the same way the Dockerfile is: there is no registry to
/// fetch it from and nothing to install on the host.
const CAIRNFLOW: &[(&str, &str)] = &[
    ("main.go", include_str!("../assets/cairnflow/main.go")),
    ("go.mod", include_str!("../assets/cairnflow/go.mod")),
    ("go.sum", include_str!("../assets/cairnflow/go.sum")),
];

/// What the provider is called inside the image.
pub const FLOW_PROVIDER: &str = "cairnflow";

/// The shared image's name. Version-tagged so upgrading cairn rebuilds it rather than
/// silently reusing indexers from an older release.
pub fn image_tag() -> String {
    format!("cairn-indexers:{}", env!("CARGO_PKG_VERSION"))
}

/// The container serving one repository.
///
/// Named from the repository's own path rather than from the directory name, so two
/// checkouts of the same project do not collide, and stable across runs so the container
/// started an hour ago is the one found now.
pub fn container_name(repo: &Path) -> String {
    let digest = blake3::hash(repo.to_string_lossy().as_bytes());
    format!("cairn-{}", &digest.to_hex()[..12])
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
    // The flow provider's source goes into the context too, so the image can compile it
    // at build time - which is the only time there is a network. A `go build` at query
    // time would need one, and D15 says a query has none.
    let provider = dir.join("cairnflow");
    std::fs::create_dir_all(&provider)
        .with_context(|| format!("creating {}", provider.display()))?;
    for (name, text) in CAIRNFLOW {
        std::fs::write(provider.join(name), text)
            .with_context(|| format!("writing {}", provider.join(name).display()))?;
    }
    let path = dir.join("Dockerfile");
    // Rewritten every time rather than only when absent: an edited or truncated file here
    // would build an image that is not the one this cairn was published with.
    std::fs::write(&path, DOCKERFILE).with_context(|| format!("writing {}", path.display()))?;
    Ok(dir)
}

/// Where the repository is mounted inside the container. Fixed, so the paths an indexer
/// records do not depend on where the repository lives on this machine.
pub const MOUNT: &str = "/repo";

/// Is the repository's container up?
fn running(name: &str) -> bool {
    Command::new("docker")
        .args(["inspect", "--format", "{{.State.Running}}", name])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false)
}

/// A volume name for one project's dependencies, stable across runs.
///
/// Keyed on the repository path and the project's path within it, so two checkouts and two
/// workspace members never share a tree.
pub fn deps_volume(repo: &Path, rel: &Path) -> String {
    let key = format!("{}::{}", repo.display(), rel.display());
    format!(
        "cairn-deps-{}",
        &blake3::hash(key.as_bytes()).to_hex()[..12]
    )
}

/// The dependency roots a repository's container must carry, remembered for this process.
///
/// `exec` restarts the container when it has gone, and has no survey to hand. Without this
/// it would recreate one with no volumes at all, and the next install would write the
/// dependency tree into the user's working copy — the failure this whole mechanism exists
/// to prevent, arriving by the one path that does not go through `index`.
static DEP_ROOTS: OnceLock<Mutex<HashMap<String, Vec<PathBuf>>>> = OnceLock::new();

fn remembered(name: &str) -> Vec<PathBuf> {
    DEP_ROOTS
        .get_or_init(Default::default)
        .lock()
        .map(|m| m.get(name).cloned().unwrap_or_default())
        .unwrap_or_default()
}

/// Start the repository's container carrying a volume for each of `dep_roots`, and return
/// its name.
///
/// The roots are relative to the repository root, and they are passed here rather than
/// arranged later because a volume cannot be added to a running container.
pub fn ensure_container_with_deps(repo: &Path, dep_roots: &[PathBuf]) -> Result<String> {
    ensure_container_inner(repo, dep_roots)
}

/// Start the repository's container if it is not already up, and return its name.
///
/// One container per repository, started once and left running — **not** a fresh
/// `docker run` per call. Language servers are the reason: gopls and pyright are
/// expensive to start and only earn their keep warm, and a container that came and went
/// with each request would throw that away every time. Indexing rides on the same
/// container rather than paying container startup twice for no reason.
///
/// It holds no state of its own. The repository is a mount and the process inside is a
/// sleep, so losing the container costs a restart and nothing else.
pub fn ensure_container(repo: &Path) -> Result<String> {
    ensure_container_inner(repo, &[])
}

/// The named volumes a container carries, or `None` if there is no such container.
///
/// The two have to be distinguishable. A container from before this mechanism carries no
/// named volumes at all, and reading that as "nothing is there" is the one mistake that
/// silently reuses it — which is exactly the container that must not be reused. Binds
/// report an empty name and drop out; only the volumes cairn manages are of interest.
fn mounted_volumes(name: &str) -> Option<Vec<String>> {
    let out = Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{range .Mounts}}{{.Name}}\n{{end}}",
            name,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

fn ensure_container_inner(repo: &Path, dep_roots: &[PathBuf]) -> Result<String> {
    let repo =
        std::fs::canonicalize(repo).with_context(|| format!("resolving {}", repo.display()))?;
    let name = container_name(&repo);

    let dep_roots: Vec<PathBuf> = if dep_roots.is_empty() {
        remembered(&name)
    } else {
        if let Ok(mut m) = DEP_ROOTS.get_or_init(Default::default).lock() {
            m.insert(name.clone(), dep_roots.to_vec());
        }
        dep_roots.to_vec()
    };
    let wanted: Vec<String> = dep_roots
        .iter()
        .map(|rel| deps_volume(repo.as_ref(), rel.as_path()))
        .collect();

    // A container that predates these volumes, or that was started for a repository whose
    // workspace has grown a member since, is missing at least one of them - and a missing
    // volume is not a smaller version of the same thing, it is the install landing in the
    // working copy. Reuse is conditional on carrying every one of them.
    match mounted_volumes(&name) {
        Some(existing) if wanted.iter().all(|v| existing.contains(v)) => {
            if running(&name) {
                return Ok(name);
            }
            // Present but stopped: start it rather than rebuilding. It holds no state, but
            // recreating it would throw away whatever the language servers have cached.
            if Command::new("docker")
                .args(["start", &name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
            {
                return Ok(name);
            }
        }
        Some(_) => {
            let _ = Command::new("docker")
                .args(["rm", "--force", &name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        None => {}
    }

    let mut cmd = Command::new("docker");
    cmd.args(["run", "--detach", "--name", &name])
        .arg("--volume")
        .arg(format!("{}:{MOUNT}", repo.display()))
        .arg("--workdir")
        .arg(MOUNT);

    // A volume over each project's `node_modules`, so the dependency tree lives where node
    // resolution needs it and nowhere on the host.
    //
    // Reading the user's own `node_modules` was the alternative and it is not one: a team
    // that builds its frontend in Docker has none, and a team that has them today can stop
    // tomorrow. A tool whose index depends on a directory it does not own breaks quietly
    // when that directory goes. cairn declares one dependency - Docker - and everything
    // else it needs, it provides.
    //
    // Docker creates a named volume owned by root, and this container runs as the caller,
    // so the first `mkdir` inside one fails with EACCES and the install dies a line into
    // resolving. Ownership is set first, by a throwaway root container - one for all of
    // them, because a monorepo has a volume per workspace member and a container each would
    // be most of the first run's wall clock.
    #[cfg(unix)]
    if !wanted.is_empty() {
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        let mut chown = Command::new("docker");
        chown.args(["run", "--rm", "--user", "0:0"]);
        let mut targets = Vec::new();
        for (n, vol) in wanted.iter().enumerate() {
            chown.arg("--volume").arg(format!("{vol}:/v{n}"));
            targets.push(format!("/v{n}"));
        }
        let _ = chown
            .arg(image_tag())
            .args(["chown", "-R", &format!("{uid}:{gid}")])
            .args(&targets)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    for (rel, vol) in dep_roots.iter().zip(&wanted) {
        cmd.arg("--volume").arg(format!(
            "{vol}:{}",
            Path::new(MOUNT).join(rel).join("node_modules").display()
        ));
    }

    // Run as the caller, so files written inside belong to them rather than to root.
    #[cfg(unix)]
    {
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        cmd.arg("--user").arg(format!("{uid}:{gid}"));
    }

    // `sleep infinity` is the whole job: a container needs a process to stay up, and this
    // one exists only to be `docker exec`-ed into.
    let out = cmd
        .arg(image_tag())
        .args(["sleep", "infinity"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("running `docker run`")?;
    if out.status.success() {
        return Ok(name);
    }

    // Someone else created it between the check and the create — two commands in the same
    // repository at once is ordinary, not exceptional. Their container is as good as ours.
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("already in use") && running(&name) {
        return Ok(name);
    }
    bail!("could not start the indexer container: {}", stderr.trim());
}

/// Run one command inside the repository's container.
pub fn exec(repo: &Path, workdir: &Path, args: &[&str]) -> Result<std::process::Output> {
    exec_in(repo, workdir, args, Stdio::null())
}

/// The same, keeping stdout.
///
/// Separate because the indexers write files and say nothing worth reading, while a
/// provider's whole answer is on stdout — and inheriting megabytes of indexer progress
/// into a query's output was not worth the one shared function.
pub fn exec_out(repo: &Path, workdir: &Path, args: &[&str]) -> Result<std::process::Output> {
    exec_in(repo, workdir, args, Stdio::piped())
}

fn exec_in(
    repo: &Path,
    workdir: &Path,
    args: &[&str],
    stdout: Stdio,
) -> Result<std::process::Output> {
    let name = ensure_container(repo)?;
    let repo = std::fs::canonicalize(repo)?;
    let rel = workdir.strip_prefix(&repo).unwrap_or(Path::new(""));

    Command::new("docker")
        .args(["exec", "--workdir"])
        .arg(Path::new(MOUNT).join(rel))
        .arg(&name)
        .args(args)
        .stdout(stdout)
        .stderr(Stdio::piped())
        .output()
        .context("running `docker exec`")
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
            DOCKERFILE.contains("scip-typescript"),
            "the TypeScript indexer is missing"
        );
        // Every language cairn claims to index must have its indexer in the image. Adding
        // a `Language` entry and forgetting the Dockerfile produces "executable file not
        // found" from inside a container, minutes into a first run.
        for lang in crate::index::LANGUAGES {
            assert!(
                DOCKERFILE.contains(lang.indexer),
                "{} is indexed by {}, which the image does not install",
                lang.name,
                lang.indexer
            );
        }
        assert!(
            DOCKERFILE.starts_with('#'),
            "expected the explanatory header"
        );
    }
}
