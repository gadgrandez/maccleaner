//! Benchmarks the space view: `cargo run --release -p maccleaner-core --example bench_space -- <path>`.
use std::path::PathBuf;

fn main() {
    let path = std::env::args().nth(1).map(PathBuf::from).or_else(maccleaner_core::rules::home_dir).expect("path");
    let r = maccleaner_core::space::space(&path, 10).expect("space");
    println!("{} bytes, {} files, {} errors, {} ms", r.total_bytes, r.total_files, r.errors, r.elapsed_ms);
}
