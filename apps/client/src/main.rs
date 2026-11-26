//! BW-15-02-03's Linux client leg: load the fixed map, display it
//! through the software device and exit.
//!
//! The reference client is a Windows/D3D9 binary (`BW client.exe`); the
//! BW-11 port carries the render device as a library whose software
//! backend needs no GPU and no display server. This entry binds that
//! library to the acceptance the ledger row states — a *fixed map*
//! displayed — by loading the reference fixture space, presenting a
//! frame series of it, and writing a TGA screenshot when asked. Every
//! frame checksum it prints is byte-deterministic, so the same run on
//! another Linux host reads the same ledger.
//!
//! ```sh
//! cargo run -p boundless-client -- \
//!     --res docs/review/fixtures/chunk_space/res/space_00010001 \
//!     --frames 8 [--shot /tmp/fixed_map]
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use client::map_view;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let mut res: Option<String> = None;
    let mut frames: u32 = 8;
    let mut shot: Option<String> = None;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        // The value is the next argument, unless it is another flag.
        let mut value = || match iter.next() {
            Some(v) if !v.starts_with("--") => Some(v.clone()),
            _ => None,
        };
        match arg.as_str() {
            "--res" => match value() {
                Some(v) => res = Some(v),
                None => {
                    eprintln!("boundless-client: --res needs a space directory");
                    return ExitCode::FAILURE;
                }
            },
            "--frames" => match value().and_then(|v| v.parse().ok()) {
                Some(v) => frames = v,
                None => {
                    eprintln!("boundless-client: --frames needs a number");
                    return ExitCode::FAILURE;
                }
            },
            "--shot" => match value() {
                Some(v) => shot = Some(v),
                None => {
                    eprintln!("boundless-client: --shot needs a file name");
                    return ExitCode::FAILURE;
                }
            },
            other => {
                eprintln!("boundless-client: unknown argument {other}");
                return ExitCode::FAILURE;
            }
        }
    }
    let space_root = match res {
        Some(res) => PathBuf::from(res),
        None => PathBuf::from("docs/review/fixtures/chunk_space/res/space_00010001"),
    };

    let run = match map_view::run_map_display(&space_root, frames) {
        Ok(run) => run,
        Err(err) => {
            eprintln!("boundless-client: {err}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "space={} bound={} width={} height={}",
        space_root.display(),
        run.bound.len(),
        run.width,
        run.height
    );
    for id in &run.bound {
        println!("bound {id}");
    }
    for frame in &run.frames {
        println!(
            "frame={} checksum={:016x}",
            frame.frame_index, frame.checksum
        );
    }
    if let Some(shot) = shot {
        match map_view::write_screenshot(&run, std::path::Path::new(&shot)) {
            Ok(written) => println!("shot={}", written.display()),
            Err(err) => {
                eprintln!("boundless-client: {err}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}
