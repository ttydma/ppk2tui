use std::io::{self, BufRead, BufReader, Read, Write};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::calibration::{adc_to_microamps, parse_modifiers, Modifiers, SpikeFilter};
use super::protocol::*;

pub struct PPK2Device<R: Read, W: Write> {
    reader: BufReader<R>,
    writer: W,
    pub modifiers: Modifiers,
    pub vdd_mv: u16,
    remainder: Vec<u8>,
    filter: SpikeFilter,
}

impl<R: Read, W: Write> PPK2Device<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
            modifiers: Modifiers::default(),
            vdd_mv: 3300,
            remainder: Vec::new(),
            filter: SpikeFilter::new(),
        }
    }

    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Fetch calibration metadata from the device.
    /// Sends GET_META_DATA and reads lines until one contains "END".
    pub fn get_modifiers(&mut self) -> Result<()> {
        self.send(&[CMD_GET_META_DATA])?;
        let mut text = String::new();
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line)?;
            let done = line.contains("END");
            text.push_str(&line);
            if done {
                break;
            }
        }
        self.modifiers = parse_modifiers(&text)?;
        Ok(())
    }

    pub fn set_mode_ampere(&mut self) -> Result<()> {
        self.send(&[CMD_SET_POWER_MODE, MODE_AMPERE])
    }

    pub fn set_mode_source(&mut self, mv: u16) -> Result<()> {
        if !(800..=5000).contains(&mv) {
            bail!("voltage {mv} mV out of range 800–5000");
        }
        self.vdd_mv = mv;
        let cmd = encode_voltage(mv);
        self.send(&cmd)?;
        self.send(&[CMD_SET_POWER_MODE, MODE_SOURCE])
    }

    pub fn set_dut_power(&mut self, on: bool) -> Result<()> {
        let param = if on { CMD_AVERAGE_START } else { CMD_NO_OP };
        self.send(&[CMD_DEVICE_RUNNING, param])
    }

    pub fn start_measuring(&mut self) -> Result<()> {
        self.send(&[CMD_AVERAGE_START])
    }

    pub fn stop_measuring(&mut self) -> Result<()> {
        self.send(&[CMD_AVERAGE_STOP])
    }

    pub fn reset(&mut self) -> Result<()> {
        self.send(&[CMD_RESET])
    }

    /// Read available bytes from the serial port, parse complete 4-byte samples,
    /// apply calibration and spike filter, and return µA values.
    pub fn read_samples(&mut self) -> Result<Vec<f32>> {
        let mut buf = [0u8; 4096];
        let n = match self.reader.read(&mut buf) {
            Ok(n) => n,
            Err(e)
                if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::WouldBlock =>
            {
                0
            }
            Err(e) => return Err(e.into()),
        };

        if n == 0 {
            return Ok(Vec::new());
        }

        self.remainder.extend_from_slice(&buf[..n]);
        let mut samples = Vec::new();
        let full = (self.remainder.len() / 4) * 4;
        for chunk in self.remainder[..full].chunks_exact(4) {
            let bytes = [chunk[0], chunk[1], chunk[2], chunk[3]];
            let (adc, range, _digital) = parse_sample_raw(bytes);
            let ua = adc_to_microamps(adc, range, self.vdd_mv, &self.modifiers);
            let filtered = self.filter.apply(ua, range);
            samples.push(filtered);
        }
        self.remainder.drain(..full);
        Ok(samples)
    }
}

/// Open a real serial port and return a PPK2Device.
pub fn open(path: &str) -> Result<PPK2Device<impl Read, impl Write>> {
    let port = serialport::new(path, 9600)
        .timeout(Duration::from_millis(50))
        .open()
        .with_context(|| format!("failed to open serial port {path}"))?;

    // serialport::SerialPort implements both Read and Write; clone for split R/W
    let writer = port
        .try_clone()
        .context("failed to clone serial port for writing")?;
    Ok(PPK2Device::new(port, writer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn make_device(input: Vec<u8>) -> PPK2Device<Cursor<Vec<u8>>, Vec<u8>> {
        PPK2Device::new(Cursor::new(input), Vec::new())
    }

    fn encode_sample(adc_raw: u16, range: u8, digital: u8) -> [u8; 4] {
        // adc field is bits 0-13 (raw value = adc/4), range bits 14-16, digital bits 17-24
        let word: u32 = (adc_raw as u32 / 4) | ((range as u32) << 14) | ((digital as u32) << 17);
        word.to_le_bytes()
    }

    #[test]
    fn get_modifiers_parses_metadata() {
        let metadata = b"R=1031.64,101.65,10.15,0.94,0.043\n\
                         O=0.0,0.0,0.0,0.0,0.0\n\
                         GS=1.0,1.0,1.0,1.0,1.0\n\
                         GI=0.0,0.0,0.0,0.0,0.0\n\
                         S=0.0,0.0,0.0,0.0,0.0\n\
                         I=0.0,0.0,0.0,0.0,0.0\n\
                         UG=1.0,1.0,1.0,1.0,1.0\n\
                         END\n";
        let mut dev = make_device(metadata.to_vec());
        dev.get_modifiers().unwrap();
        assert!((dev.modifiers.r[0] - 1031.64).abs() < 1e-2);
        assert!((dev.modifiers.r[4] - 0.043).abs() < 1e-4);
    }

    #[test]
    fn read_samples_returns_three_values() {
        let s0 = encode_sample(400, 0, 0);
        let s1 = encode_sample(400, 0, 0);
        let s2 = encode_sample(400, 0, 0);
        let mut data = Vec::new();
        data.extend_from_slice(&s0);
        data.extend_from_slice(&s1);
        data.extend_from_slice(&s2);
        let mut dev = make_device(data);
        let samples = dev.read_samples().unwrap();
        assert_eq!(samples.len(), 3);
    }

    #[test]
    fn read_samples_handles_partial_trailing_bytes() {
        let s0 = encode_sample(200, 0, 0);
        let mut data = s0.to_vec();
        data.push(0xAB); // trailing incomplete sample
        let mut dev = make_device(data);
        let samples = dev.read_samples().unwrap();
        assert_eq!(samples.len(), 1);
        assert_eq!(dev.remainder.len(), 1); // leftover byte kept
    }

    #[test]
    fn set_mode_source_writes_correct_bytes() {
        let mut dev = make_device(Vec::new());
        dev.set_mode_source(3300).unwrap();
        // Expected: encode_voltage(3300) = [0x0d, 12, 228], then [0x11, 0x02]
        assert_eq!(&dev.writer[..3], &[CMD_REGULATOR_SET, 12, 228]);
        assert_eq!(&dev.writer[3..], &[CMD_SET_POWER_MODE, MODE_SOURCE]);
    }

    #[test]
    fn set_mode_source_rejects_out_of_range() {
        let mut dev = make_device(Vec::new());
        assert!(dev.set_mode_source(799).is_err());
        assert!(dev.set_mode_source(5001).is_err());
    }

    #[test]
    fn start_stop_measuring_writes_correct_bytes() {
        let mut dev = make_device(Vec::new());
        dev.start_measuring().unwrap();
        dev.stop_measuring().unwrap();
        assert_eq!(dev.writer[0], CMD_AVERAGE_START);
        assert_eq!(dev.writer[1], CMD_AVERAGE_STOP);
    }
}
