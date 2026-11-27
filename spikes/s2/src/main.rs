//! Spike S2: shared boot image and garbage collection primitives.
//!
//! ```text
//! s2 boot [--procs N] [--size-mib M] [--base HEX] [--backing pagefile|file]
//!         [--mode self|inject|mixed] [--cow part|all] [--dirty-pages N]
//!         [--appcontainer N] [--force-reloc N]
//! s2 aslr [--runs N] [--base HEX] [--size-mib M] [--appcontainer-every K]
//! s2 gc   [--mib M] [--reps R]
//! ```

mod args;
mod boot;
mod gc;
mod win;

fn main() {
    let a = args::Args::from_env();
    let r = match a.cmd.as_str() {
        "boot" => boot::broker(&a),
        "child" => boot::child(&a),
        "aslr" => boot::aslr(&a),
        "probe" => boot::probe(&a),
        "gc" => gc::run(&a),
        _ => {
            eprintln!("usage: s2 boot|aslr|gc [options] (see src/main.rs)");
            std::process::exit(2);
        }
    };
    if let Err(e) = r {
        eprintln!("s2 {}: error: {e}", a.cmd);
        std::process::exit(1);
    }
}
