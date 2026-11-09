mod station;

use station::{Reporter, Terse, Verbose};

fn main() {
    let readings: Vec<Box<dyn Reporter>> = vec![
        Box::new(Terse {
            id: "WX-1".to_string(),
        }),
        Box::new(Verbose {
            id: "WX-2".to_string(),
            altitude: 900,
        }),
    ];
    for line in station::report_all(&readings) {
        println!("{line}");
    }
}
