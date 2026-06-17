use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::sync::mpsc::Sender;

use crate::Command;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Ampere,
    Source,
}

pub struct KeyState {
    pub dut_on: bool,
    pub mode: Mode,
    pub vdd_mv: u16,
}

/// Handle a key event, sending the appropriate Command on the channel.
/// Returns true if the application should quit.
pub fn handle_key(key: KeyEvent, state: &mut KeyState, tx: &Sender<Command>) -> bool {
    match key.code {
        KeyCode::Char('q') => {
            let _ = tx.send(Command::Quit);
            return true;
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let _ = tx.send(Command::Quit);
            return true;
        }
        KeyCode::Char('p') => {
            state.dut_on = !state.dut_on;
            let _ = tx.send(Command::SetDutPower(state.dut_on));
        }
        KeyCode::Char('r') => {
            let _ = tx.send(Command::Reset);
        }
        KeyCode::Up if state.mode == Mode::Source => {
            state.vdd_mv = (state.vdd_mv + 100).min(5000);
            let _ = tx.send(Command::SetVoltage(state.vdd_mv));
        }
        KeyCode::Down if state.mode == Mode::Source => {
            state.vdd_mv = state.vdd_mv.saturating_sub(100).max(800);
            let _ = tx.send(Command::SetVoltage(state.vdd_mv));
        }
        KeyCode::Char('s') => {
            let _ = tx.send(Command::CycleTimeScale);
        }
        _ => {}
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use std::sync::mpsc;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn key_mod(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn default_state() -> KeyState {
        KeyState {
            dut_on: false,
            mode: Mode::Ampere,
            vdd_mv: 3300,
        }
    }

    #[test]
    fn q_sends_quit_and_returns_true() {
        let (tx, rx) = mpsc::channel();
        let mut state = default_state();
        let quit = handle_key(key(KeyCode::Char('q')), &mut state, &tx);
        assert!(quit);
        assert_eq!(rx.recv().unwrap(), Command::Quit);
    }

    #[test]
    fn ctrl_c_sends_quit() {
        let (tx, rx) = mpsc::channel();
        let mut state = default_state();
        let quit = handle_key(
            key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut state,
            &tx,
        );
        assert!(quit);
        assert_eq!(rx.recv().unwrap(), Command::Quit);
    }

    #[test]
    fn p_toggles_dut_power_off_to_on() {
        let (tx, rx) = mpsc::channel();
        let mut state = default_state();
        handle_key(key(KeyCode::Char('p')), &mut state, &tx);
        assert!(state.dut_on);
        assert_eq!(rx.recv().unwrap(), Command::SetDutPower(true));
    }

    #[test]
    fn p_toggles_dut_power_on_to_off() {
        let (tx, rx) = mpsc::channel();
        let mut state = KeyState {
            dut_on: true,
            mode: Mode::Ampere,
            vdd_mv: 3300,
        };
        handle_key(key(KeyCode::Char('p')), &mut state, &tx);
        assert!(!state.dut_on);
        assert_eq!(rx.recv().unwrap(), Command::SetDutPower(false));
    }

    #[test]
    fn up_in_ampere_mode_is_ignored() {
        let (tx, rx) = mpsc::channel();
        let mut state = default_state();
        handle_key(key(KeyCode::Up), &mut state, &tx);
        assert!(
            rx.try_recv().is_err(),
            "no command should be sent in ampere mode"
        );
        assert_eq!(state.vdd_mv, 3300);
    }

    #[test]
    fn up_in_source_mode_increases_voltage() {
        let (tx, rx) = mpsc::channel();
        let mut state = KeyState {
            dut_on: false,
            mode: Mode::Source,
            vdd_mv: 3300,
        };
        handle_key(key(KeyCode::Up), &mut state, &tx);
        assert_eq!(state.vdd_mv, 3400);
        assert_eq!(rx.recv().unwrap(), Command::SetVoltage(3400));
    }

    #[test]
    fn down_in_source_mode_decreases_voltage() {
        let (tx, rx) = mpsc::channel();
        let mut state = KeyState {
            dut_on: false,
            mode: Mode::Source,
            vdd_mv: 3300,
        };
        handle_key(key(KeyCode::Down), &mut state, &tx);
        assert_eq!(state.vdd_mv, 3200);
        assert_eq!(rx.recv().unwrap(), Command::SetVoltage(3200));
    }

    #[test]
    fn voltage_clamped_at_max() {
        let (tx, rx) = mpsc::channel();
        let mut state = KeyState {
            dut_on: false,
            mode: Mode::Source,
            vdd_mv: 4950,
        };
        handle_key(key(KeyCode::Up), &mut state, &tx);
        handle_key(key(KeyCode::Up), &mut state, &tx);
        assert_eq!(state.vdd_mv, 5000);
        rx.recv().unwrap();
        rx.recv().unwrap();
    }

    #[test]
    fn voltage_clamped_at_min() {
        let (tx, rx) = mpsc::channel();
        let mut state = KeyState {
            dut_on: false,
            mode: Mode::Source,
            vdd_mv: 850,
        };
        handle_key(key(KeyCode::Down), &mut state, &tx);
        handle_key(key(KeyCode::Down), &mut state, &tx);
        assert_eq!(state.vdd_mv, 800);
        rx.recv().unwrap();
        rx.recv().unwrap();
    }
}
