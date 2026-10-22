//! Benchmarks the developer module (read-only): `cargo run --release -p maccleaner-core --example bench_dev`.
use maccleaner_core::dev;

fn gb(b: u64) -> String {
    format!("{:.2} GB", b as f64 / 1e9)
}

fn main() {
    let p = dev::scan_projects(&dev::ProjectsRequest::default()).expect("projects");
    let inactive: u64 = p.projects.iter().filter(|x| x.inactive_days >= dev::INACTIVE_DAYS).map(|x| x.bytes).sum();
    println!("projects: {} with artifacts, {} total, {} inactive (>{} days), {} dirs, {} ms",
        p.projects.len(), gb(p.total_bytes), gb(inactive), dev::INACTIVE_DAYS, p.scanned_dirs, p.elapsed_ms);
    for x in p.projects.iter().take(8) {
        println!("  {:>9}  {:>4} days  {:<28} {}", gb(x.bytes), x.inactive_days, x.ecosystems.join("+"), x.path);
    }
    let t = dev::scan_toolchains().expect("toolchains");
    println!("toolchains: {} total, {} ms", gb(t.total_bytes), t.elapsed_ms);
    for i in &t.items {
        println!("  {:>9}  {}  {}", gb(i.bytes), if i.default_selected { "[x]" } else { "[ ]" }, i.name);
    }
}
