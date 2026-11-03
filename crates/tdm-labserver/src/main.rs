use std::process::ExitCode;

use tdm_labserver::{LabConfig, Scenario, parse_size, start};

const USAGE: &str = "\
Usage: tdm-labserver [--size <bytes|KiB|MiB|GiB>] [--scenario <name>] [--port <n>] [--path <url-path>]

  --size       file size, default 16MiB
  --scenario   fast | latency:<ms> | drops:<bytes> | capped:<bytes_per_sec> |
               refuse:<n> | norange | changed | slowread | fail_every:<n>
               default fast
  --port       TCP port on 127.0.0.1, 0 = ephemeral (default 0)
  --path       URL path of the file, default /file.bin

Runs in the foreground until Ctrl+C.";

struct Args {
    size: u64,
    scenario: Scenario,
    port: u16,
    path: String,
}

fn parse_args(raw: &[String]) -> Result<Option<Args>, String> {
    let mut size = "16MiB".to_string();
    let mut scenario = "fast".to_string();
    let mut port = "0".to_string();
    let mut path = "/file.bin".to_string();

    let mut i = 0;
    while i < raw.len() {
        let arg = raw[i].as_str();
        if arg == "-h" || arg == "--help" {
            return Ok(None);
        }
        // Accept both "--flag value" and "--flag=value".
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) => (f, Some(v.to_string())),
            None => (arg, None),
        };
        let slot = match flag {
            "--size" => &mut size,
            "--scenario" => &mut scenario,
            "--port" => &mut port,
            "--path" => &mut path,
            other => return Err(format!("unknown argument '{other}'")),
        };
        match inline {
            Some(v) => *slot = v,
            None => {
                i += 1;
                *slot = raw
                    .get(i)
                    .cloned()
                    .ok_or_else(|| format!("{flag} needs a value"))?;
            }
        }
        i += 1;
    }

    Ok(Some(Args {
        size: parse_size(&size).map_err(|e| e.to_string())?,
        scenario: scenario.parse().map_err(|e: tdm_labserver::LabError| e.to_string())?,
        port: port.parse().map_err(|_| format!("bad port '{port}'"))?,
        path,
    }))
}

#[tokio::main]
async fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&raw) {
        Ok(Some(a)) => a,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let lab = match start(LabConfig {
        size_bytes: args.size,
        scenario: args.scenario,
        port: args.port,
        path: args.path,
    })
    .await
    {
        Ok(lab) => lab,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("serving {} bytes at {} (scenario {})", args.size, lab.url, args.scenario);
    println!("press Ctrl+C to stop");

    if let Err(e) = tokio::signal::ctrl_c().await {
        eprintln!("error: waiting for Ctrl+C: {e}");
        return ExitCode::FAILURE;
    }
    println!("stopping");
    match lab.shutdown().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
