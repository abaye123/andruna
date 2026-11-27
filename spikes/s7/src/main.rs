//! Spike S7: ART, D3D11 and DirectComposition inside an AppContainer, and
//! the two warm-process strategies of docs/spec/04-security.md.
//!
//! ```text
//! s7 art     [--grant aap|cap|pkg|none] [--lpac] [--no-container]
//!            [--dex hello.dex] [--class Hello] [-- <andruna-app options>]
//! s7 warm    [--runs N] [--grant cap]
//! s7 gpu     [--no-container] [--lpac] [--grant ...]
//! s7 angle   --angle-dir <dir> [--grant ...]
//! s7 pipe    [--no-ac-ace]
//! s7 section
//! s7 cleanup
//! ```
//!
//! Paths: the ART root is `out/guest/x64/art-root` (or `--art-root`),
//! andruna-app is `target/release/andruna-app.exe` of the workspace (or
//! `--app`), the test dex files are in `out/s1/java`. Everything the
//! container runs from is staged under `out/s7`.

mod args;
mod art;
mod env;
mod gpu;
mod ipc;
mod proc;
mod section;
mod win;

fn main() {
    let a = args::Args::from_env();
    let r = match a.cmd.as_str() {
        "art" => art::run(&a),
        "warm" => art::warm(&a),
        "gpu" => gpu::parent(&a),
        "gpu-child" => gpu::child(&a),
        "angle" => gpu::angle(&a),
        "angle-child" => gpu::angle_child(&a),
        "pipe" => ipc::parent(&a),
        "pipe-child" => ipc::child(&a),
        "section" => section::parent(&a),
        "section-child" => section::child(&a),
        "cleanup" => env::cleanup(&a),
        _ => {
            eprintln!(
                "usage: s7 art|warm|gpu|angle|pipe|section|cleanup [options] (see src/main.rs)"
            );
            std::process::exit(2);
        }
    };
    if let Err(e) = r {
        eprintln!("s7 {}: error: {e}", a.cmd);
        std::process::exit(1);
    }
}
