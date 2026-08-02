use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    symbols,
    text::{Line, Span},
    widgets::{Axis, Block, Borders, Chart, Dataset, Paragraph},
    Frame,
};

use super::{AppState, UnitScale};
use crate::tui::events::Mode;

// ── Unit resolution ───────────────────────────────────────────────────────────

/// Returns (unit_label, µA→unit multiplier).
/// Auto uses the larger of `avg_ua` and `max_ua` so we don't under-scale at
/// startup (empty buffer → avg=0) or miss transient peaks.
pub fn resolve_unit(scale: UnitScale, avg_ua: f32, max_ua: f32) -> (&'static str, f32) {
    match scale {
        UnitScale::Auto => {
            let r = avg_ua.abs().max(max_ua.abs());
            if r == 0.0 {
                ("µA", 1.0)
            } else if r < 1.0 {
                ("nA", 1_000.0)
            } else if r < 1_000.0 {
                ("µA", 1.0)
            } else {
                ("mA", 0.001)
            }
        }
        UnitScale::Micro => ("µA", 1.0),
        UnitScale::Milli => ("mA", 0.001),
    }
}

/// Format a µA value in the given display unit.
/// nA: integer (device resolution ~10 nA; sub-nA is noise).
/// µA: 2 dp.  mA: 3 dp.
pub fn fmt_current(ua: f32, multiplier: f32, unit: &str) -> String {
    let v = ua * multiplier;
    match unit {
        "nA" => format!("{:.0} nA", v.max(0.0)),
        "mA" => format!("{:.3} mA", v),
        _ => format!("{:.2} µA", v),
    }
}

/// Pick the best unit for a single µA value (always auto-scales per value).
fn best_unit(ua: f32) -> (&'static str, f32) {
    let abs = ua.abs();
    if abs == 0.0 || (1.0..1_000.0).contains(&abs) {
        ("µA", 1.0)
    } else if abs < 1.0 {
        ("nA", 1_000.0)
    } else {
        ("mA", 0.001)
    }
}

/// Color for a given unit label — makes mixed-unit stat lines visually scannable.
fn unit_color(unit: &str) -> Color {
    match unit {
        "nA" => Color::Yellow,
        "mA" => Color::Green,
        _ => Color::Cyan, // µA
    }
}

/// A colored Span for a single µA value, auto-scaled to its own best unit.
fn value_span(ua: f32) -> Span<'static> {
    let (unit, mult) = best_unit(ua);
    Span::styled(
        fmt_current(ua, mult, unit),
        Style::default().fg(unit_color(unit)),
    )
}

/// Format an already-converted display value (not µA) with the right precision.
fn fmt_display(val: f64, unit: &str) -> String {
    match unit {
        "nA" => format!("{:.0} {unit}", val.max(0.0)),
        "mA" => format!("{:.3} {unit}", val),
        _ => format!("{:.1} {unit}", val),
    }
}

// ── Main draw ─────────────────────────────────────────────────────────────────

/// `sess` = (avg_ua, min_ua, max_ua, sample_count) since DUT last powered on.
pub fn draw(frame: &mut Frame, state: &AppState, samples: &[f32], sess: (f32, f32, f32, u64)) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),    // chart
            Constraint::Length(3), // rolling stats
            Constraint::Length(3), // DUT session stats
            Constraint::Length(1), // status bar
        ])
        .split(area);

    let (inst, avg1, avg10, min_ua, max_ua) = compute_stats(samples);
    // Use the abs-max of the visible window so negative calibration offsets don't
    // trick Auto into picking nA (fold starting at 0 would return 0 for all-negative data).
    let visible_abs_max = samples
        .iter()
        .rev()
        .take((state.time_scale_secs as usize) * 100_000)
        .copied()
        .fold(0.0f32, |acc, v| acc.max(v.abs()));
    let (unit, multiplier) = resolve_unit(state.unit_scale, avg1, visible_abs_max);

    // ── Chart ────────────────────────────────────────────────────────────────

    let window = (state.time_scale_secs as usize) * 100_000;
    let start = samples.len().saturating_sub(window);
    let visible = &samples[start..];
    let n = visible.len(); // samples we actually have (≤ window)

    // y_max anchored to ALL visible samples so the scale never shrinks because a
    // downsampled bucket missed the true peak.
    let y_max_ua = visible
        .iter()
        .copied()
        .fold(0.0f32, |a, v| a.max(v))
        .max(0.0);
    let y_max = nice_ceil((y_max_ua as f64 * multiplier as f64).max(1.0));
    let y_mid = y_max / 2.0;

    // Fixed bucket grid anchored to `window`, not to `n`.
    //
    // `offset` = how many samples are still "missing" from the left edge (ring
    // buffer not yet full for this scale). Each bucket's x-coord is computed
    // from its position inside the full window and never changes between frames,
    // so the chart doesn't jitter or drift as new samples arrive.
    let max_pts = 500usize;
    let bucket_size = (window / max_pts).max(1);
    let offset = window - n; // samples absent from left edge

    // Chart inner height (rows available for data, inside borders and x-axis label).
    // Used to compute the fill step: one point per pixel row from 0 → peak.
    let chart_rows = chunks[0].height.saturating_sub(4).max(1) as usize;
    let y_step = (y_max / chart_rows as f64).max(f64::EPSILON);

    // Build filled-area data: for each bucket emit one point per pixel row from
    // y=0 up to the peak.  With Marker::Block this fills the column solid.
    let mut chart_data: Vec<(f64, f64)> = Vec::with_capacity(max_pts * chart_rows);
    for i in 0..max_pts {
        let wb_start = i * bucket_size;
        let wb_end = ((i + 1) * bucket_size).min(window);
        if wb_end <= offset {
            continue;
        }

        let s = wb_start.saturating_sub(offset);
        let e = wb_end.saturating_sub(offset).min(n);
        if s >= n {
            continue;
        }

        let peak = visible[s..e]
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        if !peak.is_finite() {
            continue;
        }

        let t = (wb_end as f64 - window as f64) / 100_000.0;
        let peak_display = (peak.max(0.0) * multiplier) as f64;
        let rows = ((peak_display / y_step).ceil() as usize).min(chart_rows);

        for row in 0..=rows {
            chart_data.push((t, (row as f64 * y_step).min(peak_display)));
        }
    }

    let x_min = -(state.time_scale_secs as f64);

    // Human-readable window label: show "Xm Ys" for ≥60 s windows
    let window_label = fmt_duration(state.time_scale_secs);

    let dataset = Dataset::default()
        .marker(symbols::Marker::Block)
        .style(Style::default().fg(Color::Cyan))
        .data(&chart_data);

    let chart = Chart::new(vec![dataset])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Current Draw — {} window ", window_label))
                // Right-aligned in the top border so the running build is always
                // identifiable without leaving the TUI.
                .title(
                    Line::from(Span::styled(
                        format!(" {} ", crate::build_info::TUI_LABEL),
                        Style::default().fg(Color::DarkGray),
                    ))
                    .right_aligned(),
                ),
        )
        .x_axis(Axis::default().bounds([x_min, 0.0]).labels(vec![
            format!("-{}", window_label),
            format!("-{}", fmt_duration(state.time_scale_secs / 2)),
            "now".to_string(),
        ]))
        .y_axis(Axis::default().bounds([0.0, y_max]).labels(vec![
            "0".to_string(),
            fmt_display(y_mid, unit),
            fmt_display(y_max, unit),
        ]));
    frame.render_widget(chart, chunks[0]);

    // ── Rolling stats (each value picks its own unit + color) ───────────────

    let rolling = Line::from(vec![
        Span::raw("  Now: "),
        value_span(inst),
        Span::raw("  |  Avg 1s: "),
        value_span(avg1),
        Span::raw("  |  Avg 10s: "),
        value_span(avg10),
        Span::raw("  |  Min: "),
        value_span(min_ua),
        Span::raw("  |  Max: "),
        value_span(max_ua),
        Span::raw(format!("  |  n: {}", samples.len())),
    ]);
    frame.render_widget(
        Paragraph::new(rolling).block(Block::default().borders(Borders::ALL).title(" Rolling ")),
        chunks[1],
    );

    // ── DUT session stats ────────────────────────────────────────────────────

    // ── DUT session stats (each value picks its own unit + color) ───────────

    let (sess_avg, sess_min, sess_max, sess_n) = sess;
    let session_line = if !state.dut_on && sess_n == 0 {
        Line::from(Span::raw("  DUT off — no session data"))
    } else {
        Line::from(vec![
            Span::raw("  Avg: "),
            value_span(sess_avg),
            Span::raw("  |  Min: "),
            value_span(sess_min),
            Span::raw("  |  Max: "),
            value_span(sess_max),
            Span::raw(format!(
                "  |  n: {}  {}",
                sess_n,
                if state.dut_on {
                    "(running)"
                } else {
                    "(last session)"
                },
            )),
        ])
    };
    let sess_title = if state.dut_on {
        " DUT Session ● "
    } else {
        " DUT Session "
    };
    frame.render_widget(
        Paragraph::new(session_line).block(
            Block::default()
                .borders(Borders::ALL)
                .title(sess_title)
                .style(if state.dut_on {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default()
                }),
        ),
        chunks[2],
    );

    // ── Status bar ───────────────────────────────────────────────────────────

    let mode_str = match state.mode {
        Mode::Ampere => "Ampere".to_string(),
        Mode::Source => format!("Source {}mV", state.vdd_mv),
    };
    let dut_str = if state.dut_on { "DUT: ON" } else { "DUT: OFF" };
    let unit_str = format!("unit: {}", state.unit_scale.label());
    let log_str = match &state.log_path {
        Some(p) => format!(" ● LOG:{p}"),
        None => String::new(),
    };
    let status = Line::from(vec![
        Span::styled(
            format!(
                " {} | {} | {} | {}{} ",
                state.port, mode_str, dut_str, unit_str, log_str
            ),
            Style::default().fg(Color::Green),
        ),
        Span::raw("  [q] Quit  [p] DUT  [s] Scale  [u] Unit  [↑↓] Voltage"),
    ]);
    frame.render_widget(Paragraph::new(status), chunks[3]);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn compute_stats(samples: &[f32]) -> (f32, f32, f32, f32, f32) {
    if samples.is_empty() {
        return (0.0, 0.0, 0.0, 0.0, 0.0);
    }
    let inst = *samples.last().unwrap();
    let avg1 = mean(&samples[samples.len().saturating_sub(100_000)..]);
    let avg10 = mean(&samples[samples.len().saturating_sub(1_000_000)..]);
    let min_ua = samples.iter().copied().fold(f32::INFINITY, f32::min);
    let max_ua = samples.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    (inst, avg1, avg10, min_ua, max_ua)
}

fn mean(s: &[f32]) -> f32 {
    if s.is_empty() {
        0.0
    } else {
        s.iter().sum::<f32>() / s.len() as f32
    }
}

/// Round `value` up to the nearest "nice" number: 1, 2, or 5 × 10ⁿ.
/// Examples: 347 → 500, 85 → 100, 12 → 20, 2.3 → 5, 0.85 → 1.
fn nice_ceil(value: f64) -> f64 {
    if value <= 0.0 {
        return 1.0;
    }
    let exp = value.log10().floor();
    let magnitude = 10f64.powi(exp as i32);
    let fraction = value / magnitude;
    let nice = if fraction <= 1.0 {
        1.0
    } else if fraction <= 2.0 {
        2.0
    } else if fraction <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * magnitude
}

fn fmt_duration(secs: u16) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        let m = secs / 60;
        let s = secs % 60;
        if s == 0 {
            format!("{m}m")
        } else {
            format!("{m}m{s}s")
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_label_renders_in_chart_header() {
        use ratatui::{backend::TestBackend, Terminal};

        let state = AppState {
            port: "/dev/ttyACM0".to_string(),
            mode: Mode::Ampere,
            vdd_mv: 3300,
            dut_on: false,
            time_scale_secs: 5,
            unit_scale: UnitScale::Auto,
            log_path: None,
        };

        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal
            .draw(|f| draw(f, &state, &[1.0, 2.0, 3.0], (0.0, 0.0, 0.0, 0)))
            .unwrap();

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();

        assert!(
            rendered.contains(crate::build_info::TUI_LABEL),
            "build label {:?} missing from rendered TUI",
            crate::build_info::TUI_LABEL
        );
    }

    #[test]
    fn narrow_terminal_does_not_panic() {
        use ratatui::{backend::TestBackend, Terminal};

        let state = AppState {
            port: "/dev/ttyACM0".to_string(),
            mode: Mode::Ampere,
            vdd_mv: 3300,
            dut_on: false,
            time_scale_secs: 5,
            unit_scale: UnitScale::Auto,
            log_path: None,
        };

        // The chart title plus the build label is ~57 columns; narrower
        // terminals must truncate rather than panic.
        for width in [40u16, 60, 80] {
            let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
            terminal
                .draw(|f| draw(f, &state, &[1.0, 2.0, 3.0], (0.0, 0.0, 0.0, 0)))
                .unwrap();
        }
    }

    #[test]
    fn auto_no_data_defaults_to_ua() {
        let (unit, _) = resolve_unit(UnitScale::Auto, 0.0, 0.0);
        assert_eq!(unit, "µA");
    }

    #[test]
    fn auto_picks_na_for_sub_ua() {
        let (unit, mult) = resolve_unit(UnitScale::Auto, 0.5, 0.8);
        assert_eq!(unit, "nA");
        assert!((mult - 1_000.0).abs() < 1e-6);
    }

    #[test]
    fn auto_picks_ua_for_mid_range() {
        let (unit, mult) = resolve_unit(UnitScale::Auto, 250.0, 400.0);
        assert_eq!(unit, "µA");
        assert!((mult - 1.0).abs() < 1e-6);
    }

    #[test]
    fn auto_picks_ma_above_1000ua() {
        let (unit, mult) = resolve_unit(UnitScale::Auto, 1500.0, 2000.0);
        assert_eq!(unit, "mA");
        assert!((mult - 0.001).abs() < 1e-9);
    }

    #[test]
    fn auto_uses_visible_max_not_just_avg() {
        let (unit, _) = resolve_unit(UnitScale::Auto, 0.0, 5000.0);
        assert_eq!(unit, "mA");
    }

    #[test]
    fn explicit_milli_overrides_auto() {
        let (unit, _) = resolve_unit(UnitScale::Milli, 0.001, 0.001);
        assert_eq!(unit, "mA");
    }

    #[test]
    fn fmt_na_is_integer() {
        assert_eq!(fmt_current(0.5, 1_000.0, "nA"), "500 nA");
    }

    #[test]
    fn fmt_na_clamps_negative_to_zero() {
        assert_eq!(fmt_current(-5.0, 1_000.0, "nA"), "0 nA");
    }

    #[test]
    fn fmt_ma_three_dp() {
        assert_eq!(fmt_current(1500.0, 0.001, "mA"), "1.500 mA");
    }

    #[test]
    fn fmt_ua_two_dp() {
        assert_eq!(fmt_current(123.456, 1.0, "µA"), "123.46 µA");
    }

    #[test]
    fn nice_ceil_rounds_to_1_2_5_multiples() {
        assert_eq!(nice_ceil(347.0), 500.0); // 3.47 × 100 → 5 × 100
        assert_eq!(nice_ceil(85.0), 100.0); // 8.5  × 10  → 10 × 10
        assert_eq!(nice_ceil(12.0), 20.0); // 1.2  × 10  → 2  × 10
        assert_eq!(nice_ceil(2.3), 5.0); // 2.3  × 1   → 5  × 1
        assert_eq!(nice_ceil(0.85), 1.0); // 8.5  × 0.1 → 10 × 0.1
        assert_eq!(nice_ceil(1.0), 1.0); // exact
        assert_eq!(nice_ceil(500.0), 500.0); // exact
        assert_eq!(nice_ceil(501.0), 1000.0); // just over 500
    }
}
