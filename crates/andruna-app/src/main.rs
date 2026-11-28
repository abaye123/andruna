//! `andruna-app`: starts ART on andruna-linker (see the library
//! documentation and the crate README).

use andruna_app::{Action, ArtRoot, Layout, USAGE, exe_dir, execute, parse, plan};

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
    let art = ArtRoot::locate(opts.art_root.as_deref(), exe_dir().as_deref())?;
    let layout = Layout::new(art, opts.data.as_deref());
    let classpath = match &opts.action {
        Action::Run { classpath, .. } | Action::Compile { classpath } => classpath.clone(),
        Action::CompileBoot => vec![],
    };
    let p = plan(&opts, &layout)?;
    layout.install(&classpath)?;
    andruna_linker::profile::mark("guest tree mounted");
    if std::env::var_os("ANDRUNA_APP_TRACE").is_some() {
        eprintln!("andruna-app: {} {}", p.exe.display(), p.argv.join(" "));
    }
    match execute(p)? {}
}
