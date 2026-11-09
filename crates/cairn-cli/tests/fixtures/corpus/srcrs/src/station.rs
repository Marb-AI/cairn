//! Station report formatting - the firmware that runs on a weather station itself,
//! which is why it is Rust rather than Go or Python in this corpus: resource-constrained
//! edge devices are the one part of a telemetry network teams actually reach for it.
//!
//! The Rust analogue of `srcgo`'s `station.Reporter`: the reason `unreached`/`outline`
//! have to parse an `impl#[Type][Trait]method()` symbol instead of reading a real SCIP
//! relationship the way they do for Go and Python. `rust-analyzer scip` emits no
//! relationships of its own - checked at the raw protobuf level, not just its JSON
//! rendering - so the fact has to come from the symbol string via
//! `cairn_scip::rust_trait_impl_target` instead.

pub trait Reporter {
    fn report(&self) -> String;
}

pub struct Terse {
    pub id: String,
}

impl Reporter for Terse {
    fn report(&self) -> String {
        self.id.clone()
    }
}

/// Never named directly anywhere in this crate - the whole point of the fixture.
pub struct Verbose {
    pub id: String,
    pub altitude: i64,
}

impl Reporter for Verbose {
    fn report(&self) -> String {
        if self.altitude >= 800 {
            format!("{} (elevated)", self.id)
        } else {
            self.id.clone()
        }
    }
}

/// Calls each reporter through the trait object. Which concrete type runs is not
/// knowable without running the program - that is what the trait object is for.
pub fn report_all(rs: &[Box<dyn Reporter>]) -> Vec<String> {
    rs.iter().map(|r| r.report()).collect()
}

/// Genuinely uncalled, next to a dispatched one: the contrast case. The fix must not
/// start calling everything reachable - only what a trait impl actually dispatches.
///
/// Takes no `Terse`/`Verbose` and calls neither's `report`: either would be a second,
/// statically-resolvable caller next to the trait-object one in `report_all`, which
/// would make that impl method merely under-counted rather than purely dispatched -
/// the wrong shape for this contrast.
pub fn orphaned_summary(id: &str) -> String {
    format!("orphan: {id}")
}
