//! `andruna-app`: starts ART on andruna-linker (see the library
//! documentation and the crate README).

use andruna_app::{
    Action, ArtRoot, Layout, USAGE, connect_compositor, exe_dir, execute, mounted_entries, parse,
    plan,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 1 && args[0] == "--host" {
        std::process::exit(andruna_app::host::run());
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return;
    }
    if let Err(e) = start(&args) {
        eprintln!("andruna-app: {e}");
        std::process::exit(2);
    }
}

fn start(args: &[String]) -> Result<(), String> {
    andruna_linker::profile::mark("andruna-app main");
    let opts = parse(args).map_err(|e| format!("{e}\n{USAGE}"))?;
    let art = match ArtRoot::locate(opts.art_root.as_deref(), exe_dir().as_deref()) {
        Ok(a) => a,
        // A native program needs no ART; its tree gets an empty ART
        // directory (bionic then comes from the library search path).
        Err(_) if matches!(opts.action, Action::Exec { .. }) && opts.art_root.is_none() => {
            ArtRoot::empty_in(opts.data.as_deref())?
        }
        Err(e) => return Err(e),
    };
    let layout = Layout::new(art, opts.data.as_deref());
    let entries = mounted_entries(&opts.action);
    let p = plan(&opts, &layout)?;
    layout.install(&entries)?;
    andruna_linker::profile::mark("guest tree mounted");
    if let Some(h) = &opts.compositor {
        connect_compositor(h)?;
        andruna_linker::profile::mark("compositor connected");
    }
    if std::env::var_os("ANDRUNA_APP_TRACE").is_some() {
        eprintln!("andruna-app: {} {}", p.exe.display(), p.argv.join(" "));
    }
    match execute(p)? {}
}
