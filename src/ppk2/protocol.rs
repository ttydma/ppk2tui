pub const CMD_NO_OP: u8 = 0x00;
pub const CMD_AVERAGE_START: u8 = 0x06;
pub const CMD_AVERAGE_STOP: u8 = 0x07;
#[allow(dead_code)]
pub const CMD_RANGE_SET: u8 = 0x08;
pub const CMD_DEVICE_RUNNING: u8 = 0x0c;
pub const CMD_REGULATOR_SET: u8 = 0x0d;
pub const CMD_SET_POWER_MODE: u8 = 0x11;
pub const CMD_GET_META_DATA: u8 = 0x19;
pub const CMD_RESET: u8 = 0x20;

pub const MODE_AMPERE: u8 = 0x01;
pub const MODE_SOURCE: u8 = 0x02;

/// Encode a source voltage (mV) into the 3-byte REGULATOR_SET payload.
/// Valid range: 800–5000 mV.
pub fn encode_voltage(mv: u16) -> [u8; 3] {
    let diff = (mv as u32).saturating_sub(800) + 32;
    let b1 = (3 + (diff >> 8)) as u8;
    let b2 = (diff & 0xFF) as u8;
    [CMD_REGULATOR_SET, b1, b2]
}

/// Parse a 4-byte raw sample into (adc_value, range_index, digital_channels).
/// Layout (little-endian u32):
///   bits  0–13: 14-bit ADC (must multiply by 4 to get full-scale equivalent)
///   bits 14–16: range index 0–4
///   bits 17–24: 8 digital channel bits
pub fn parse_sample_raw(bytes: [u8; 4]) -> (u16, u8, u8) {
    let word = u32::from_le_bytes(bytes);
    let adc = ((word & 0x3FFF) as u16) * 4;
    let range = ((word >> 14) & 0x07) as u8;
    let digital = ((word >> 17) & 0xFF) as u8;
    (adc, range, digital)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_voltage_min() {
        let b = encode_voltage(800);
        assert_eq!(b[0], CMD_REGULATOR_SET);
        // diff = 0 + 32 = 32; b1 = 3 + 0 = 3; b2 = 32
        assert_eq!(b[1], 3);
        assert_eq!(b[2], 32);
    }

    #[test]
    fn encode_voltage_3300() {
        let b = encode_voltage(3300);
        // diff = 2500 + 32 = 2532; b1 = 3 + (2532>>8) = 3 + 9 = 12; b2 = 2532 & 0xFF = 228
        assert_eq!(b[0], CMD_REGULATOR_SET);
        assert_eq!(b[1], 12);
        assert_eq!(b[2], 228);
    }

    #[test]
    fn encode_voltage_max() {
        let b = encode_voltage(5000);
        // diff = 4200 + 32 = 4232; b1 = 3 + (4232>>8) = 3 + 16 = 19; b2 = 4232 & 0xFF = 136
        assert_eq!(b[0], CMD_REGULATOR_SET);
        assert_eq!(b[1], 19);
        assert_eq!(b[2], 136);
    }

    #[test]
    fn parse_sample_adc_only() {
        // adc=100 (raw 25 in bits 0-13), range=0, digital=0
        let word: u32 = 25u32; // 25 * 4 = 100
        let (adc, range, digital) = parse_sample_raw(word.to_le_bytes());
        assert_eq!(adc, 100);
        assert_eq!(range, 0);
        assert_eq!(digital, 0);
    }

    #[test]
    fn parse_sample_with_range() {
        // range = 3, adc = 0, digital = 0
        let word: u32 = 3u32 << 14;
        let (adc, range, digital) = parse_sample_raw(word.to_le_bytes());
        assert_eq!(adc, 0);
        assert_eq!(range, 3);
        assert_eq!(digital, 0);
    }

    #[test]
    fn parse_sample_with_digital() {
        // digital = 0b10101010, range = 1, adc = 0
        let word: u32 = (1u32 << 14) | (0b10101010u32 << 17);
        let (adc, range, digital) = parse_sample_raw(word.to_le_bytes());
        assert_eq!(adc, 0);
        assert_eq!(range, 1);
        assert_eq!(digital, 0b10101010);
    }

    #[test]
    fn parse_sample_all_fields() {
        // adc raw = 200 → adc = 800, range = 2, digital = 0xFF
        let word: u32 = 200u32 | (2u32 << 14) | (0xFFu32 << 17);
        let (adc, range, digital) = parse_sample_raw(word.to_le_bytes());
        assert_eq!(adc, 800);
        assert_eq!(range, 2);
        assert_eq!(digital, 0xFF);
    }
}
