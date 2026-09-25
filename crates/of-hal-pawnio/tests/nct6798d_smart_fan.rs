//! The chip's own fan curves, decoded from a real register dump.
//!
//! Register addresses come from the Linux `nct6775` driver; these tests are what makes
//! them *ours* — evidence that the addresses describe this chip, on this board, rather than
//! a table copied in good faith. The values asserted below were read off the dump by hand
//! before the decoder existed, so the decoder is checked against the bytes and not
//! against itself.

#![cfg(windows)]

use std::collections::BTreeMap;

use of_hal_pawnio::nct6775::{
    FanMode, REG_AUTO_TEMP, REG_FAN_MODE, SMART_FAN_POINTS, SmartFanCurve, decode_smart_fan_curve,
    mode_from_register, smart_fan_registers, temp_source_label,
};

const FIXTURE: &str = include_str!("fixtures/nct6798d-quasar.txt");

fn registers(fixture: &str) -> BTreeMap<u16, u8> {
    let mut map = BTreeMap::new();
    for line in fixture.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((position, bytes)) = line.split_once(' ') else {
            continue;
        };
        let Some((bank, offset)) = position.split_once(':') else {
            continue;
        };
        let bank = u16::from_str_radix(bank, 16).expect("bank");
        let offset = u16::from_str_radix(offset, 16).expect("offset");
        for (i, byte) in bytes.split_whitespace().enumerate() {
            if let Ok(value) = u8::from_str_radix(byte, 16) {
                map.insert((bank << 8) | (offset + i as u16), value);
            }
        }
    }
    map
}

fn curve(regs: &BTreeMap<u16, u8>, channel: usize) -> Option<SmartFanCurve> {
    decode_smart_fan_curve(channel, |register| regs.get(&register).copied())
}

#[test]
fn the_board_curve_on_the_channel_still_under_firmware_decodes_to_a_real_ladder() {
    // Channel 0 was the one channel in SmartFan IV at capture time — the BIOS still had
    // it — so this is the closest thing in the dump to a curve the firmware wrote.
    let regs = registers(FIXTURE);
    let c = curve(&regs, 0).expect("channel 0 is fully captured");

    assert_eq!(
        mode_from_register(regs[&REG_FAN_MODE[0]]),
        FanMode::SmartFanIv
    );

    let temps: Vec<u8> = c.points.iter().map(|p| p.temp_c).collect();
    let duties: Vec<u8> = c.points.iter().map(|p| p.duty).collect();
    assert_eq!(temps, [30, 70, 70, 100]);
    assert_eq!(duties, [153, 255, 255, 255]);
    assert!(c.is_monotonic());

    // 0xFF is full speed, not an unset slot: this curve reaches 100 % at 70 °C and holds.
    assert_eq!(c.points[1].duty, 255);
}

#[test]
fn a_channel_another_tool_had_taken_still_carries_its_curve() {
    // Channel 1 was in Manual — software had taken it — and its curve registers are
    // intact. Whatever took it moved the mode nibble and left the ladder alone. That is
    // the evidence behind the plan's claim that a takeover does not destroy the curve.
    let regs = registers(FIXTURE);
    let c = curve(&regs, 1).expect("channel 1 is fully captured");

    assert_eq!(mode_from_register(regs[&REG_FAN_MODE[1]]), FanMode::Manual);

    let temps: Vec<u8> = c.points.iter().map(|p| p.temp_c).collect();
    let duties: Vec<u8> = c.points.iter().map(|p| p.duty).collect();
    assert_eq!(temps, [20, 65, 70, 100]);
    assert_eq!(duties, [51, 178, 255, 255]);
    assert!(c.is_monotonic());
}

#[test]
fn every_captured_channel_has_a_monotonic_ladder() {
    // The strongest cheap evidence that the addresses are right: a temperature ladder
    // that went backwards would mean reading something that is not a curve. The original
    // capture covers banks 0-7, so exactly three channels are decodable from it.
    let regs = registers(FIXTURE);
    let decoded: Vec<(usize, SmartFanCurve)> = (0..7)
        .filter_map(|ch| curve(&regs, ch).map(|c| (ch, c)))
        .collect();

    assert_eq!(
        decoded.iter().map(|(ch, _)| *ch).collect::<Vec<_>>(),
        [0, 1, 2],
        "banks 8-B are not in this capture, so channels 3-6 must decode to None"
    );
    for (ch, c) in &decoded {
        assert!(c.is_monotonic(), "channel {ch}: {c:?}");
    }
}

#[test]
fn a_missing_register_yields_no_curve_rather_than_a_default() {
    // Channels 3-6 live in banks the original capture did not include. The decoder must
    // say so, not invent zeros: a curve with a hole filled by a default is the exact
    // failure this project refuses elsewhere.
    let regs = registers(FIXTURE);
    for ch in 3..7 {
        assert_eq!(curve(&regs, ch), None, "channel {ch}");
    }
}

#[test]
fn the_cpu_fan_follows_a_source_this_crate_does_not_expose_yet() {
    // The finding worth a test of its own. Both channels the BIOS treats as CPU-related
    // follow source 28 — "PECI Agent 0 Calibration" in the driver's table — which on this
    // AMD board is evidently how the firmware routes CPU temperature. Our TEMP_INPUTS has
    // SYSTIN (1) and CPUTIN (2) only, so a graph cannot bind the same reading the BIOS
    // uses. Anything that offloads a curve has to reckon with that.
    let regs = registers(FIXTURE);

    let ch0 = curve(&regs, 0).unwrap();
    let ch1 = curve(&regs, 1).unwrap();
    assert_eq!(ch0.temp_source(), 28);
    assert_eq!(ch1.temp_source(), 28);
    assert_eq!(temp_source_label(28), Some("PECI Agent 0 Calibration"));

    // And channel 2 follows a thermistor pin instead.
    let ch2 = curve(&regs, 2).unwrap();
    assert_eq!(ch2.temp_source(), 3);
    assert_eq!(temp_source_label(3), Some("AUXTIN0"));
}

#[test]
fn the_rest_of_the_channel_configuration_reads_as_plausible_firmware_values() {
    let regs = registers(FIXTURE);
    let c = curve(&regs, 0).unwrap();

    // 125 °C critical on the CPU-related channels; a thermistor channel got 100 °C.
    assert_eq!(c.critical_temp, 125);
    assert_eq!(curve(&regs, 2).unwrap().critical_temp, 100);
    // Stop/start duties of 1 and a 60-unit stop time: the board's defaults, not ours.
    assert_eq!((c.start_output, c.stop_output, c.stop_time), (1, 1, 60));
    // Channel 2 alone has step pacing configured.
    assert_eq!(curve(&regs, 2).unwrap().step_up_time, 10);
    assert_eq!(c.step_up_time, 0);
}

#[test]
fn the_record_before_write_set_covers_every_register_the_decoder_reads() {
    // If the decoder ever reads a register that is not in the record set, a restore would
    // silently miss it. Tie the two together so they cannot drift.
    let regs = registers(FIXTURE);
    let mut touched = std::collections::BTreeSet::new();
    let _ = decode_smart_fan_curve(0, |register| {
        touched.insert(register);
        regs.get(&register).copied()
    });

    let recorded: std::collections::BTreeSet<u16> = smart_fan_registers(0).into_iter().collect();
    assert_eq!(touched, recorded);
    assert_eq!(recorded.len(), 9 + 2 * SMART_FAN_POINTS);
    assert!(recorded.contains(&REG_AUTO_TEMP[0]));
}

#[test]
fn source_labels_the_driver_leaves_blank_are_not_invented() {
    assert_eq!(temp_source_label(0), None);
    assert_eq!(temp_source_label(12), None);
    assert_eq!(temp_source_label(1), Some("SYSTIN"));
    assert_eq!(temp_source_label(2), Some("CPUTIN"));
    // Only the low five bits select; high bits are something else and must not shift the
    // lookup.
    assert_eq!(temp_source_label(0b1110_0010), Some("CPUTIN"));
}

// A second capture, taken 2026-09-25, covering all sixteen banks — so every one of the
// seven channels is present. The banks-0-7 fixture above still earns its keep: it is what
// proves a *missing* channel decodes to None rather than to zeros.
const FULL_FIXTURE: &str = include_str!("fixtures/nct6798d-quasar-full.txt");

#[test]
fn all_seven_channels_decode_from_a_full_capture() {
    let regs = registers(FULL_FIXTURE);
    let curves: Vec<SmartFanCurve> = (0..7)
        .map(|ch| curve(&regs, ch).unwrap_or_else(|| panic!("channel {ch} should be present")))
        .collect();

    for (ch, c) in curves.iter().enumerate() {
        assert!(
            c.is_monotonic(),
            "channel {ch} ladder goes backwards: {c:?}"
        );
        assert!(
            c.points.iter().all(|p| p.temp_c <= 120),
            "channel {ch} has an implausible curve temperature: {c:?}"
        );
    }

    // The CPU-related channels route through the PECI calibration source on this AMD
    // board; the chassis/thermistor channels use AUXTIN0. Neither the driver nor we
    // invent a meaning the chip does not give a slot.
    for (ch, c) in curves.iter().enumerate() {
        let source = c.temp_source();
        assert!(
            temp_source_label(source).is_some(),
            "channel {ch} follows source {source}, which has no documented label"
        );
    }
}
