use std::env;
use std::ffi::OsStr;
use std::io::{self, Write, BufRead, BufReader};
use std::path::PathBuf;
use std::process::{self, Command, Stdio};
use std::time::Instant;
use regex::Regex;

use Cfg;
use errors::*;
use notifications::*;
use rustup_utils;
use telemetry::{Telemetry, TelemetryEvent};


pub fn run_command_for_dir<S: AsRef<OsStr>>(cmd: Command,
                                            args: &[S],
                                            cfg: &Cfg) -> Result<()> {
    let arg0 = env::args().next().map(|a| PathBuf::from(a));
    let arg0 = arg0.as_ref()
        .and_then(|a| a.file_name())
        .and_then(|a| a.to_str());
    let arg0 = try!(arg0.ok_or(ErrorKind::NoExeName));
    if (arg0 == "rustc" || arg0 == "rustc.exe") && cfg.telemetry_enabled() {
        if (&args).iter().any(|e| {
            let e = e.as_ref().to_str().unwrap_or("");
            e == "--version" || e == "-V"
        }) {
            return telemetetry_rustc_version(cmd, &args, &cfg);
        }

        return telemetry_rustc(cmd, &args, &cfg);
    }
    
    run_command_for_dir_without_telemetry(cmd, &args)
}

fn telemetetry_rustc_version<S: AsRef<OsStr>>(mut cmd: Command, args: &[S], cfg: &Cfg) -> Result<()> {
    cmd.args(&args[1..]);

    let mut cmd = cmd.stdin(Stdio::inherit())
                     .stdout(Stdio::piped())
                     .stderr(Stdio::inherit())
                     .spawn()
                     .unwrap();

    let mut buffered_stdout = BufReader::new(cmd.stdout.take().unwrap());
    let status = cmd.wait();

    let t = Telemetry::new(cfg.multirust_dir.join("telemetry"));

    match status {
        Ok(status) => {
            let exit_code = status.code().unwrap_or(1);

            let re = Regex::new(r"^\w+ (?P<version>\d+\..*) \((?P<hash>.*) (?P<release>\d{4}-\d{2}-\d{2})").unwrap();

            let mut buffer = String::new();

            let stdout = io::stdout();
            let mut handle = stdout.lock();

            while buffered_stdout.read_line(&mut buffer).unwrap() > 0 {
                let b = buffer.to_owned();
                buffer.clear();
                let _ = handle.write(b.as_bytes());

                let c = re.captures(&b);

                match c {
                    None => continue,
                    Some(caps) => {
                        if caps.len() > 0 {
                            let te = TelemetryEvent::RustcVersion { version: caps.name("version").unwrap_or("").to_owned(), 
                                                                    version_hash: caps.name("hash").unwrap_or("").to_owned(), 
                                                                    build_date: caps.name("release").unwrap_or("").to_owned() };
                            let _ = t.log_telemetry(te).map_err(|xe| {
                                cfg.notify_handler.call(Notification::TelemetryCleanupError(&xe));
                            });
                        }
                    }
                };
            }

            process::exit(exit_code);
        },
        Err(e) => {
            Err(e).chain_err(|| rustup_utils::ErrorKind::RunningCommand {
                name: args[0].as_ref().to_owned(),
            })
        },
    }
}

fn telemetry_rustc<S: AsRef<OsStr>>(mut cmd: Command, args: &[S], cfg: &Cfg) -> Result<()> {
    let now = Instant::now();

    cmd.args(&args[1..]);

    // FIXME rust-lang/rust#32254. It's not clear to me
    // when and why this is needed.
    let mut cmd = cmd.stdin(Stdio::inherit())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();

    let mut buffered_stderr = BufReader::new(cmd.stderr.take().unwrap());
    let status = cmd.wait();

    let duration = now.elapsed();

    let ms = (duration.as_secs() as u64 * 1000) + (duration.subsec_nanos() as u64 / 1000 / 1000);

    let t = Telemetry::new(cfg.multirust_dir.join("telemetry"));

    match status {
        Ok(status) => {
            let exit_code = status.code().unwrap_or(1);

            let re = Regex::new(r"\[(?P<error>E.{4})\]").unwrap();

            let mut buffer = String::new();
            let mut errors: Vec<String> = Vec::new();

            let stderr = io::stderr();
            let mut handle = stderr.lock();

            while buffered_stderr.read_line(&mut buffer).unwrap() > 0 {
                let b = buffer.to_owned();
                buffer.clear();                
                let _ = handle.write(b.as_bytes());

                let c = re.captures(&b);
                match c {
                    None => continue,
                    Some(caps) => {
                        if caps.len() > 0 {
                            let _ = errors.push(caps.name("error").unwrap_or("").to_owned());
                        }
                    }
                };
            }

            let e = match errors.len() { 
                0 => None,
                _ => Some(errors),
            };

            let te = TelemetryEvent::RustcRun { duration_ms: ms, 
                                                exit_code: exit_code,
                                                errors: e };
            
            let _ = t.log_telemetry(te).map_err(|xe| {
                cfg.notify_handler.call(Notification::TelemetryCleanupError(&xe));
            });

            process::exit(exit_code);
        },
        Err(e) => {
            let exit_code = e.raw_os_error().unwrap_or(1);
            let te = TelemetryEvent::RustcRun { duration_ms: ms,
                                                exit_code: exit_code,
                                                errors: None };
            
            let _ = t.log_telemetry(te).map_err(|xe| {
                cfg.notify_handler.call(Notification::TelemetryCleanupError(&xe));
            });

            Err(e).chain_err(|| rustup_utils::ErrorKind::RunningCommand {
                name: args[0].as_ref().to_owned(),
            })
        },
    }
}

fn run_command_for_dir_without_telemetry<S: AsRef<OsStr>>(mut cmd: Command, args: &[S]) -> Result<()>  {
    cmd.args(&args[1..]);

    // FIXME rust-lang/rust#32254. It's not clear to me
    // when and why this is needed.
    cmd.stdin(process::Stdio::inherit());

    match cmd.status() {
        Ok(status) => {
            // Ensure correct exit code is returned
            let code = status.code().unwrap_or(1);
            process::exit(code);
        }
        Err(e) => {
            Err(e).chain_err(|| rustup_utils::ErrorKind::RunningCommand {
                name: args[0].as_ref().to_owned(),
            })
        }
    }    
}
