//! ART through andruna-app inside an AppContainer, and the warm-process
//! measurements.

use std::io;
use std::time::{Duration, Instant};

use crate::args::Args;
use crate::env::{self, Env, Grant};
use crate::proc::{ms, quote, stats, Proc};
use crate::section;
use crate::win::Container;

fn app_cmdline(
    env: &Env,
    data: &std::path::Path,
    opts: &[String],
    cp: &str,
    class: &str,
) -> String {
    let mut v = vec![
        quote(&env.app().display().to_string()),
        "--art-root".into(),
        quote(&env.art_root.display().to_string()),
        "--data".into(),
        quote(&data.display().to_string()),
    ];
    v.extend(opts.iter().map(|o| quote(o)));
    v.push("--classpath".into());
    v.push(quote(cp));
    v.push(class.into());
    v.join(" ")
}

/// `s7 art`: one andruna-app run, in a container unless `--no-container`.
pub fn run(a: &Args) -> io::Result<()> {
    let env = Env::new(a);
    let grant = Grant::parse(&a.str("--grant", "cap"))?;
    let lpac = a.flag("--lpac");
    let p = env::prepare(&env, grant, lpac, "art")?;
    let (files, dirs) = env::count_tree(&env.art_root);
    println!(
        "profile {} sid {} (create/open {:.2} ms); grant {grant:?}{} on the runtime dirs ({files} files, {dirs} dirs in the ART root) {:.1} ms; data dir ACL + low label {:.2} ms",
        env::PROFILE,
        p.sid.string(),
        p.profile_ms,
        if lpac { " (LPAC)" } else { "" },
        p.grant_ms,
        p.data_ms
    );
    let dex = env.dex().join(a.str("--dex", "hello.dex"));
    let cmd = app_cmdline(
        &env,
        &p.data,
        &a.tail,
        &dex.display().to_string(),
        &a.str("--class", "Hello"),
    );
    let caps = [p.runtime_cap.clone()];
    let c = Container {
        sid: &p.sid,
        capabilities: &caps,
        lpac,
    };
    let container = if a.flag("--no-container") {
        None
    } else {
        Some(&c)
    };
    println!("cmd: {cmd}");
    let mut proc = Proc::spawn(&cmd, container, false, &[])?;
    let first = proc.wait_line("", Duration::from_secs(120));
    let code = proc.wait(Duration::from_secs(120));
    let end = Instant::now();
    println!(
        "exit {code:?}; first stdout line after {:.1} ms; exit after {:.1} ms",
        first.map(|(t, _)| ms(proc.started, t)).unwrap_or(f64::NAN),
        ms(proc.started, end)
    );
    println!("--- stdout\n{}--- stderr\n{}", proc.out(), proc.err());
    Ok(())
}

struct Sample {
    to_hello: f64,
    to_exit: f64,
}

fn once(
    cmd: &str,
    container: Option<&Container>,
    suspended_map: Option<&section::BootSections>,
) -> io::Result<Sample> {
    let mut p = Proc::spawn(cmd, container, suspended_map.is_some(), &[])?;
    if let Some(b) = suspended_map {
        b.map_into(&p.child.process, section::INJECT_BASE)?;
        p.resume();
    }
    let hello = p
        .wait_line("Hello, Andruna", Duration::from_secs(60))
        .ok_or_else(|| io::Error::other(format!("no Hello: {}{}", p.out(), p.err())))?;
    let code = p.wait(Duration::from_secs(60));
    if code != Some(0) {
        return Err(io::Error::other(format!("exit {code:?}: {}", p.err())));
    }
    Ok(Sample {
        to_hello: ms(p.started, hello.0),
        to_exit: p.started.elapsed().as_secs_f64() * 1e3,
    })
}

/// `s7 warm`: start times for cold launches outside and inside the
/// container, launch into a suspended container process with the boot
/// image sections mapped by the parent (strategy 2), and a warm host in
/// the container handed its app over stdin (strategy 1).
pub fn warm(a: &Args) -> io::Result<()> {
    let env = Env::new(a);
    let runs: usize = a.num("--runs", 10);
    let p = env::prepare(&env, Grant::parse(&a.str("--grant", "cap"))?, false, "warm")?;
    let caps = [p.runtime_cap.clone()];
    let c = Container {
        sid: &p.sid,
        capabilities: &caps,
        lpac: false,
    };
    let opts: Vec<String> = a.tail.clone();
    let hello = env.dex().join("hello.dex").display().to_string();
    let cold = app_cmdline(&env, &p.data, &opts, &hello, "Hello");
    let boot = section::BootSections::open(&env.art_root)?;

    // Page cache warm-up.
    once(&cold, None, None)?;
    once(&cold, Some(&c), None)?;

    // The variants run interleaved, so load changes on the machine affect
    // them alike.
    let names = [
        "cold, no container",
        "cold, container",
        "suspended in container + boot map (strategy 2)",
    ];
    let mut h = vec![vec![]; 3];
    let mut e = vec![vec![]; 3];
    for _ in 0..runs {
        for (i, s) in [
            once(&cold, None, None)?,
            once(&cold, Some(&c), None)?,
            once(&cold, Some(&c), Some(&boot))?,
        ]
        .into_iter()
        .enumerate()
        {
            h[i].push(s.to_hello);
            e[i].push(s.to_exit);
        }
    }
    for i in 0..3 {
        println!(
            "{:<48} to Hello {}   to exit {}",
            names[i],
            stats(&h[i]),
            stats(&e[i])
        );
    }

    // Strategy 1: a warm host in the container gets its app on stdin.
    let cp = format!("{};{}", env.dex().join("warm.dex").display(), hello);
    let warm_cmd = app_cmdline(&env, &p.data, &opts, &cp, "Warm");
    let mut ready = vec![];
    let mut handoff = vec![];
    let mut ws = vec![];
    let mut commit = vec![];
    for _ in 0..runs {
        let mut w = Proc::spawn(&warm_cmd, Some(&c), false, &[])?;
        let r = w
            .wait_line("warm: ready", Duration::from_secs(60))
            .ok_or_else(|| io::Error::other(format!("warm host not ready: {}", w.err())))?;
        ready.push(ms(w.started, r.0));
        // Let the runtime's startup work (JIT thread, GC) settle as it would
        // while the host waits.
        std::thread::sleep(Duration::from_millis(300));
        let (s, pc) = w.memory();
        ws.push(s as f64 / 1048576.0);
        commit.push(pc as f64 / 1048576.0);
        let t0 = Instant::now();
        w.write(b"Hello\n")?;
        let h = w
            .wait_line("Hello, Andruna", Duration::from_secs(60))
            .ok_or_else(|| io::Error::other("warm host did not run Hello"))?;
        handoff.push(ms(t0, h.0));
        let code = w.wait(Duration::from_secs(60));
        if code != Some(0) {
            return Err(io::Error::other(format!("warm exit {code:?}: {}", w.err())));
        }
    }
    println!(
        "{:<44} {}",
        "warm host (strategy 1): start to ready",
        stats(&ready)
    );
    println!(
        "{:<44} {}",
        "warm host (strategy 1): assignment to Hello",
        stats(&handoff)
    );
    let med = |v: &[f64]| {
        let mut s = v.to_vec();
        crate::proc::median(&mut s)
    };
    println!(
        "warm host idle: working set {:.1} MiB, private commit {:.1} MiB (medians)",
        med(&ws),
        med(&commit)
    );
    Ok(())
}
