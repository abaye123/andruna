//! The sysd pipe from inside an AppContainer: the named pipe of
//! andruna-ipc with `PipeSecurity::allow_app_container`, and an inherited
//! anonymous pipe handle.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::io::FromRawHandle;
use std::time::Duration;

use andruna_ipc::{proto, Client, Endpoint, PipeListener, PipeSecurity, Request, Response};

use crate::args::Args;
use crate::env::{self, Env, Grant};
use crate::proc::{quote, Proc};
use crate::win::{self, Container};

const INSTANCE: &str = "s7test";

pub fn parent(a: &Args) -> io::Result<()> {
    let env = Env::new(a);
    let p = env::prepare(&env, Grant::Cap, false, "pipe")?;
    let endpoint = Endpoint::for_current_user(INSTANCE)?;
    let mut sec = PipeSecurity::current_user_only(&endpoint);
    if !a.flag("--no-ac-ace") {
        sec.allow_app_container(&p.sid.string())?;
    }
    println!("pipe {} sddl {}", endpoint.pipe_name(), sec.sddl());
    let mut listener = PipeListener::bind(&endpoint, &sec)?;
    let server = std::thread::spawn(move || {
        // Two connections: the raw open and the andruna-ipc client.
        for _ in 0..2 {
            let Ok(mut c) = listener.accept() else { return };
            while let Ok(Some(f)) = c.read_frame() {
                let reply = match Request::decode(&f) {
                    Ok((id, _)) => Response::Pong { protocol: 1 }.encode(id),
                    Err(_) => b"?".to_vec(),
                };
                if c.write_frame(&reply).is_err() {
                    break;
                }
            }
            println!("server: peer pid {} done", c.peer_pid());
        }
    });
    // An anonymous pipe passed by inheritance (no name, no DACL check).
    let (r, w) = win::pipe(true)?;
    let caps = [p.runtime_cap.clone()];
    let c = Container {
        sid: &p.sid,
        capabilities: &caps,
        lpac: false,
    };
    let mut ch = Proc::spawn(
        &format!(
            "{} pipe-child --pipe {} --inherited {}",
            quote(&env.s7().display().to_string()),
            quote(&endpoint.pipe_name()),
            w.0 .0 as usize
        ),
        Some(&c),
        false,
        &[w.0],
    )?;
    drop(w);
    let mut f = {
        let h = r.0;
        std::mem::forget(r);
        // SAFETY: ownership moves to the File.
        unsafe { File::from_raw_handle(h.0) }
    };
    let mut got = String::new();
    let _ = f.read_to_string(&mut got);
    let code = ch.wait(Duration::from_secs(30));
    println!("inherited pipe got: {got:?}");
    println!(
        "child exit {code:?}\n--- child stdout\n{}--- child stderr\n{}",
        ch.out(),
        ch.err()
    );
    // The server thread may still block in accept when a connection
    // failed; it ends with the process.
    std::thread::sleep(Duration::from_millis(100));
    drop(server);
    Ok(())
}

pub fn child(a: &Args) -> io::Result<()> {
    let (ac, il) = win::own_token();
    println!("token: appcontainer={ac} integrity=0x{il:x}");
    if let Some(h) = a.opt("--inherited") {
        let h: usize = h.parse().unwrap();
        // SAFETY: the parent passed this inherited handle value.
        let mut f = unsafe { File::from_raw_handle(h as *mut std::ffi::c_void) };
        let r = f.write_all(b"hello over an inherited pipe");
        println!("inherited pipe write: {r:?}");
    }
    let name = a.str("--pipe", "");
    // Raw open with the flags andruna-ipc uses.
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&name)
    {
        Ok(mut f) => {
            proto::write_frame(&mut f, &Request::Ping.encode(7))?;
            let r = proto::read_frame(&mut f)?;
            println!(
                "named pipe raw open: ok, answer {:?}",
                r.map(|p| Response::decode(&p).map(|x| x.1))
            );
        }
        Err(e) => println!("named pipe raw open: {e}"),
    }
    let ep = Endpoint::for_current_user(INSTANCE)?;
    println!("endpoint seen from the container: {}", ep.pipe_name());
    match Client::connect(&ep, Duration::from_secs(2)) {
        Ok(mut c) => println!(
            "andruna-ipc Client::connect: ok, ping {:?}",
            c.request(&Request::Ping)
        ),
        Err(e) => println!("andruna-ipc Client::connect: {e}"),
    }
    Ok(())
}
