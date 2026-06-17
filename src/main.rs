mod ppk2;
mod tui;

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};

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
#[command(name = "ppk2tui", about = "TUI for Nordic Semiconductor PPK2")]
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

    #[arg(short, long, help = "Log samples to CSV file (avg/min/max per 100 ms)")]
    log: Option<String>,
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

    let serial_thread = thread::spawn(move || -> Result<()> {
        let mut dev =
            ppk2::device::open(&port_path).with_context(|| format!("cannot open {port_path}"))?;

        dev.get_modifiers()
            .context("failed to read device metadata")?;

        match init_mode {
            Mode::Ampere => dev.set_mode_ampere()?,
            Mode::Source => dev.set_mode_source(vdd_mv)?,
        }

        dev.start_measuring()?;

        // Optional CSV log: one row per 100 ms bucket
        let mut log: Option<BufWriter<File>> = if let Some(ref path) = log_path {
            let mut f = BufWriter::new(
                File::create(path).with_context(|| format!("cannot create log {path}"))?,
            );
            writeln!(f, "elapsed_ms,avg_ua,min_ua,max_ua,n_samples")?;
            Some(f)
        } else {
            None
        };

        let start = Instant::now();
        let mut log_bucket_start = start;
        let mut log_sum = 0f64;
        let mut log_min = f32::INFINITY;
        let mut log_max = f32::NEG_INFINITY;
        let mut log_n = 0u32;

        let mut dut_on = false;

        loop {
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    Command::Quit => {
                        dev.stop_measuring().ok();
                        dev.reset().ok();
                        if let Some(ref mut w) = log {
                            w.flush().ok();
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
                        log_sum += s as f64;
                        log_min = log_min.min(s);
                        log_max = log_max.max(s);
                        log_n += 1;
                    }
                    if log_bucket_start.elapsed() >= Duration::from_millis(100) && log_n > 0 {
                        let elapsed_ms = start.elapsed().as_millis();
                        let avg = log_sum / log_n as f64;
                        writeln!(w, "{elapsed_ms},{avg:.2},{log_min:.2},{log_max:.2},{log_n}")?;
                        log_sum = 0.0;
                        log_min = f32::INFINITY;
                        log_max = f32::NEG_INFINITY;
                        log_n = 0;
                        log_bucket_start = Instant::now();
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
