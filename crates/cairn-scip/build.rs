// The SCIP schema is vendored (proto/scip.proto) rather than fetched, so builds
// are hermetic. Regenerate by replacing that file from github.com/sourcegraph/scip.

/// Point prost-build at a compiler, preferring one the caller named.
///
/// `PROTOC` wins so a distribution can supply its own. Otherwise we use the vendored
/// binary, which is what makes this crate build on a release runner that has nothing
/// installed. The vendored set does not cover every host (Windows on ARM is missing),
/// so a lookup failure falls through to whatever is on PATH rather than aborting the
/// build with an error about a binary the user never asked for.
fn set_protoc() {
    if std::env::var_os("PROTOC").is_some() {
        return;
    }
    if let Ok(path) = protoc_bin_vendored::protoc_bin_path() {
        std::env::set_var("PROTOC", path);
    }
}

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=proto/scip.proto");
    println!("cargo:rerun-if-env-changed=PROTOC");
    set_protoc();
    prost_build::compile_protos(&["proto/scip.proto"], &["proto"])
}
