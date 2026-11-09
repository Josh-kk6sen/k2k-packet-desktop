// ── k2k-headless — the browser-served K2K backend, no GUI toolkit ─────────
//
// Same backend, same client, same data files as the desktop app; the only
// difference is that this binary is built without Tauri (`--no-default-
// features`), so it links no GTK/WebKitGTK and installs on a lite image
// (Raspberry Pi OS, a DigiPi, a headless server) in seconds instead of
// pulling ~350 MB of GUI dependencies.
//
//   k2k-headless                      serve http://0.0.0.0:8080/
//   k2k-headless --port 8081          …on another port
//   k2k-headless --bind 127.0.0.1     …loopback only
//
// Data (settings.json, pbbs_data.json, history.json, logs/) lives in
// $XDG_DATA_HOME/K2KDesktop or ~/.local/share/K2KDesktop, exactly like the
// desktop build, so the two front-ends share one configuration.
//
// LAN tool: no TLS, no password. Bind to 127.0.0.1 or put it behind a reverse
// proxy if it ever needs to leave the local network.

const USAGE: &str = "\
k2k-headless — K2K Packet Desktop, browser-served (no GUI toolkit)

Usage:
  k2k-headless [--port <PORT>] [--bind <ADDR>]

Options:
  -p, --port <PORT>   TCP port to serve on          (default 8080)
  -b, --bind <ADDR>   Address to bind               (default 0.0.0.0)
  -h, --help          Show this help
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut port: u16 = 8080;
    let mut bind = String::from("0.0.0.0");

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) => (f, Some(v.to_string())),
            None => (a, None),
        };
        let mut take_value = |name: &str| -> Option<String> {
            let _ = name;
            if let Some(v) = inline.clone() {
                return Some(v);
            }
            i += 1;
            args.get(i).cloned()
        };
        match flag {
            "--port" | "-p" => match take_value("--port").and_then(|v| v.parse::<u16>().ok()) {
                Some(p) => port = p,
                None => fail("--port needs a number between 1 and 65535"),
            },
            "--bind" | "-b" => match take_value("--bind") {
                Some(v) if !v.is_empty() => bind = v,
                _ => fail("--bind needs an address, e.g. 0.0.0.0 or 127.0.0.1"),
            },
            "--help" | "-h" => {
                print!("{USAGE}");
                return;
            }
            other => {
                // A bare number is a convenient shorthand: `k2k-headless 8080`.
                match other.parse::<u16>() {
                    Ok(p) => port = p,
                    Err(_) => fail(&format!("Unknown option: {other}")),
                }
            }
        }
        i += 1;
    }

    if let Err(e) =
        k2k_packet_desktop_lib::run_web(k2k_packet_desktop_lib::WebOptions { port, bind })
    {
        eprintln!("k2k-headless failed: {e}");
        std::process::exit(1);
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}\n\n{USAGE}");
    std::process::exit(2);
}
