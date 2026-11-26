//! Spike S4: thread pointer and TLS options for guest code on Windows.
//!
//! Usage: `andruna-spike-s4 [probe|bench|x18-clobber|all]` (default `all`).
//! Throwaway code; see docs/plan/spike-results/S4.md for the results.

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("spike S4 supports x86_64 and aarch64 only");

mod bench;
mod hogs;
mod veh;

#[cfg(target_arch = "aarch64")]
mod arm64;
#[cfg(target_arch = "x86_64")]
mod x64;

#[cfg(target_arch = "aarch64")]
use arm64 as arch;
#[cfg(target_arch = "x86_64")]
use x64 as arch;

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    machine();
    match cmd.as_str() {
        "probe" => arch::probe(),
        "bench" => arch::benches(),
        #[cfg(target_arch = "aarch64")]
        "x18-clobber" => arm64::x18_clobber(),
        "all" => {
            arch::probe();
            arch::benches();
        }
        other => {
            eprintln!("unknown command {other}");
            std::process::exit(2);
        }
    }
}

fn machine() {
    use windows::Win32::System::SystemInformation::{GetNativeSystemInfo, SYSTEM_INFO};
    let mut si = SYSTEM_INFO::default();
    // SAFETY: GetNativeSystemInfo fills a caller-provided struct.
    unsafe { GetNativeSystemInfo(&mut si) };
    println!(
        "logical processors: {}, build target: {}",
        si.dwNumberOfProcessors,
        std::env::consts::ARCH
    );
}
