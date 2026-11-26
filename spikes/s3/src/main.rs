//! Spike S3: the Windows transport under a shared-memory Binder.
//!
//! Usage:
//!   s3 [broker] [--appcontainer] [--iters N] [--cpus a,b,c,d]
//!   s3 probe
//! The broker starts the two clients itself (`s3 client ...`).

mod broker;
mod client;
mod probe;
mod ring;
mod win;
mod winx;
mod wire;

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("broker");
    let r = match cmd {
        "client" => client::run(client::Args {
            pipe: opt(&args, "--pipe").expect("--pipe"),
            role: opt(&args, "--role").unwrap_or_else(|| "caller".into()),
            id: opt(&args, "--id").and_then(|v| v.parse().ok()).unwrap_or(0),
            peer: opt(&args, "--peer")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            iters: opt(&args, "--iters")
                .and_then(|v| v.parse().ok())
                .unwrap_or(20_000),
        }),
        "probe" => probe::run_parent(),
        "probe-child" => probe::run_child(args.get(1).expect("name")),
        _ => broker::run(broker::Args {
            appcontainer: args.iter().any(|a| a == "--appcontainer"),
            iters: opt(&args, "--iters")
                .and_then(|v| v.parse().ok())
                .unwrap_or(20_000),
            cpus: opt(&args, "--cpus")
                .map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect()),
        }),
    };
    if let Err(e) = r {
        eprintln!("s3 {cmd}: error: {e}");
        std::process::exit(1);
    }
}
