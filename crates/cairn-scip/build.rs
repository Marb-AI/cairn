// The SCIP schema is vendored (proto/scip.proto) rather than fetched, so builds
// are hermetic. Regenerate by replacing that file from github.com/sourcegraph/scip.
fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=proto/scip.proto");
    prost_build::compile_protos(&["proto/scip.proto"], &["proto"])
}
