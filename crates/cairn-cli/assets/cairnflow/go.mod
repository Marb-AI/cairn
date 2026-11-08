// The flow provider's own module. Built into the indexer image, where the Go toolchain
// already lives for scip-go; never built on a user's machine.
module cairn.local/cairnflow

go 1.24.0

require golang.org/x/tools v0.38.0
