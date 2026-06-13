pub mod events;
pub mod ui;

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::CrosstermBackend;
use ratatui::Terminal;

use crate::Command;
use events::{handle_key, KeyState, Mode};

// ── Unit scale ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnitScale {
    Auto,  // picks nA/µA/mA automatically based on signal magnitude
    Micro,
    Milli,
}

impl UnitScale {
    pub fn label(self) -> &'static str {
        match self {
            UnitScale::Auto  => "Auto",
            UnitScale::Micro => "µA",
            UnitScale::Milli => "mA",
        }
    }
}

// ── DUT session stats ─────────────────────────────────────────────────────────

/// Running min/max/avg accumulator for the current DUT power-on session.
/// Reset whenever DUT is toggled on.
pub struct SessionStats {
    pub sum:   f64,
    pub min:   f32,
    pub max:   f32,
    pub count: u64,
}

impl SessionStats {
    pub fn new() -> Self {
        Self { sum: 0.0, min: f32::INFINITY, max: f32::NEG_INFINITY, count: 0 }
    }

    pub fn update(&mut self, ua: f32) {
        self.sum   += ua as f64;
        self.count += 1;
        self.min    = self.min.min(ua);
        self.max    = self.max.max(ua);
    }

    pub fn avg(&self) -> f32 {
        if self.count == 0 { 0.0 } else { (self.sum / self.count as f64) as f32 }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

// ── App state ─────────────────────────────────────────────────────────────────

pub struct AppState {
    pub port:            String,
    pub mode:            Mode,
    pub vdd_mv:          u16,
    pub dut_on:          bool,
    pub time_scale_secs: u16,
    pub unit_scale:      UnitScale,
    pub log_path:        Option<String>,
}

impl AppState {
    pub fn cycle_time_scale(&mut self) {
        self.time_scale_secs = match self.time_scale_secs {
            1   => 5,
            5   => 10,
            10  => 30,
            30  => 60,
            60  => 300,
            300 => 600,
            _   => 1,
        };
    }

    pub fn cycle_unit_scale(&mut self) {
        self.unit_scale = match self.unit_scale {
            UnitScale::Auto  => UnitScale::Micro,
            UnitScale::Micro => UnitScale::Milli,
            UnitScale::Milli => UnitScale::Auto,
        };
    }
}

// ── TUI run loop ──────────────────────────────────────────────────────────────

pub fn run(
    ring:    Arc<Mutex<VecDeque<f32>>>,
    session: Arc<Mutex<SessionStats>>,
    tx:      std::sync::mpsc::Sender<Command>,
    mut app: AppState,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut key_state = KeyState {
        dut_on: app.dut_on,
        mode:   app.mode,
        vdd_mv: app.vdd_mv,
    };

    loop {
        // Only copy what's needed: the visible window plus enough for 10-second stats.
        // Avoids copying up to 240 MB on long time scales.
        let window = (app.time_scale_secs as usize) * 100_000;
        let stats_cap = window.max(1_000_000);
        let samples: Vec<f32> = {
            let guard = ring.lock().unwrap();
            let skip = guard.len().saturating_sub(stats_cap);
            guard.iter().skip(skip).copied().collect()
        };
        let sess_snap = {
            let g = session.lock().unwrap();
            (g.avg(), g.min, g.max, g.count)
        };

        terminal.draw(|f| ui::draw(f, &app, &samples, sess_snap))?;

        if event::poll(Duration::from_millis(60))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    let cycle_time = key.code == KeyCode::Char('s');
                    let cycle_unit = key.code == KeyCode::Char('u');
                    if handle_key(key, &mut key_state, &tx) {
                        break;
                    }
                    app.dut_on = key_state.dut_on;
                    app.vdd_mv = key_state.vdd_mv;
                    if cycle_time { app.cycle_time_scale(); }
                    if cycle_unit { app.cycle_unit_scale(); }
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
