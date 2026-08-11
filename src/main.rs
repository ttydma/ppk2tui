mod build_info;
mod logging;
mod ppk2;
mod tui;

use std::collections::VecDeque;
use std::fs::File;
use std::io::BufWriter;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};

use logging::{samples_per_bucket, BucketLogger};
use tui::events::Mode;
use tui::{AppState, SessionStats, UnitScale};

const RING_CAP: usize = 60_000_000; // 10 min at 100 kHz (~240 MB)

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Quit,
    SetDutPower(bool),
    SetVoltage(u16),
    Reset,
    CycleTimeScale,
}

#[derive(Debug, Clone, ValueEnum)]
enum CliMode {
    Ampere,
    Source,
}

#[derive(Debug, Parser)]
#[command(
    name = "ppk2tui",
    about = "TUI for Nordic Semiconductor PPK2",
    version = build_info::VERSION
)]
struct Args {
    #[arg(short, long, help = "Serial port (e.g. /dev/ttyACM0)")]
    port: String,

    #[arg(short, long, default_value = "ampere", help = "Measurement mode")]
    mode: CliMode,

    #[arg(
        short,
        long,
        default_value_t = 3300,
        help = "Source voltage in mV (800–5000, source mode only)"
    )]
    voltage: u16,

    #[arg(short, long, help = "Log samples to CSV file (avg/min/max per bucket)")]
    log: Option<String>,

    #[arg(
        long,
        default_value_t = 100_000,
        value_parser = clap::value_parser!(u64).range(1..=60_000_000),
        help = "CSV bucket size in µs; 10 logs every sample at 100 kSps"
    )]
    log_interval_us: u64,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let mode = match args.mode {
        CliMode::Ampere => Mode::Ampere,
        CliMode::Source => Mode::Source,
    };

    let ring: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::with_capacity(RING_CAP)));
    let ring_clone = Arc::clone(&ring);

    let session: Arc<Mutex<SessionStats>> = Arc::new(Mutex::new(SessionStats::new()));
    let session_clone = Arc::clone(&session);

    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();

    let port_path = args.port.clone();
    let vdd_mv = args.voltage;
    let init_mode = mode;
    let log_path = args.log.clone();
    let log_interval_us = args.log_interval_us;

    let serial_thread = thread::spawn(move || -> Result<()> {
        let mut dev =
            ppk2::device::open(&port_path).with_context(|| format!("cannot open {port_path}"))?;

        dev.get_modifiers()
            .context("failed to read device metadata")?;

        match init_mode {
            Mode::Ampere => dev.set_mode_ampere()?,
            Mode::Source => dev.set_mode_source(vdd_mv)?,
        }

        // The PPK2 keeps whatever DUT power state it was left in, so a session
        // that ended without a clean shutdown leaves the output enabled. The UI
        // starts at "DUT: OFF", so force the device to match rather than assume.
        dev.set_dut_power(false)?;

        dev.start_measuring()?;

        let mut log: Option<BucketLogger<BufWriter<File>>> = if let Some(ref path) = log_path {
            let f = BufWriter::new(
                File::create(path).with_context(|| format!("cannot create log {path}"))?,
            );
            Some(BucketLogger::new(f, samples_per_bucket(log_interval_us))?)
        } else {
            None
        };

        let mut dut_on = false;

        loop {
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    Command::Quit => {
                        dev.stop_measuring().ok();
                        dev.reset().ok();
                        if let Some(ref mut w) = log {
                            w.finish().ok();
                        }
                        return Ok(());
                    }
                    Command::SetDutPower(on) => {
                        dev.set_dut_power(on)?;
                        dut_on = on;
                        if on {
                            session_clone.lock().unwrap().reset();
                        }
                    }
                    Command::SetVoltage(mv) => dev.set_mode_source(mv)?,
                    Command::Reset => dev.reset()?,
                    Command::CycleTimeScale => {}
                }
            }

            let samples = dev.read_samples()?;
            if !samples.is_empty() {
                {
                    let mut guard = ring_clone.lock().unwrap();
                    for &s in &samples {
                        if guard.len() == RING_CAP {
                            guard.pop_front();
                        }
                        guard.push_back(s);
                    }
                }

                if dut_on {
                    let mut sess = session_clone.lock().unwrap();
                    for &s in &samples {
                        sess.update(s);
                    }
                }

                if let Some(ref mut w) = log {
                    for &s in &samples {
                        w.push(s)?;
                    }
                }
            } else {
                thread::sleep(Duration::from_millis(1));
            }
        }
    });

    let app = AppState {
        port: args.port,
        mode,
        vdd_mv: args.voltage,
        dut_on: false,
        time_scale_secs: 5,
        unit_scale: UnitScale::Auto,
        log_path: args.log,
    };

    tui::run(ring, session, cmd_tx.clone(), app)?;

    cmd_tx.send(Command::Quit).ok();
    serial_thread.join().ok();

    Ok(())
}
