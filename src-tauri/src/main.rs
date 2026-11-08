// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

const USAGE: &str = "\
K2K Packet Desktop

Usage:
  k2k-packet-desktop                     Open the desktop window (normal app)
  k2k-packet-desktop --web [options]     Serve the same client to a browser,
                                         with no window at all (headless)

Options for --web:
  -p, --port <PORT>   TCP port to serve on          (default 8080)
  -b, --bind <ADDR>   Address to bind               (default 0.0.0.0)
  -h, --help          Show this help

The browser client reads and writes the same settings, history, message board
and logs as the desktop app. Intended for a trusted local network: there is no
password and no TLS — put it behind a reverse proxy if it ever leaves the LAN.
";

fn main() {
    // Disable WebKit overlay scrollbars so CSS ::-webkit-scrollbar{width} takes effect
    #[cfg(target_os = "linux")]
    std::env::set_var("WEBKIT_DISABLE_OVERLAY_SCROLLBARS", "1");

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut web = false;
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
            "--web" | "--serve" | "-w" => web = true,
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
                // A bare port is a convenient shorthand: `--web 8080`.
                if web && other.parse::<u16>().is_ok() {
                    port = other.parse().unwrap();
                } else {
                    fail(&format!("Unknown option: {other}"));
                }
            }
        }
        i += 1;
    }

    if web {
        attach_parent_console();
        if let Err(e) =
            k2k_packet_desktop_lib::run_web(k2k_packet_desktop_lib::WebOptions { port, bind })
        {
            eprintln!("K2K Packet Desktop web mode failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    k2k_packet_desktop_lib::run()
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}\n\n{USAGE}");
    std::process::exit(2);
}

/// The release exe is built for the Windows GUI subsystem, so it has no
/// console of its own: launched from cmd, `--web` would print the URL into
/// nothing. Attaching the parent console makes the banner visible. The URL is
/// also always written to `web-client.txt` in the data folder, so it is
/// findable even when this is unavailable.
#[cfg(target_os = "windows")]
fn attach_parent_console() {
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(target_os = "windows"))]
fn attach_parent_console() {}
