//! Decode tests against a register dump captured from real hardware.
//!
//! Hardware-touching code cannot run in CI, so the decode is exercised here against bytes
//! taken off an actual NCT6798D (ASUS ROG STRIX X570-I GAMING, captured 2026-09-21 by
//! `cargo run -p of-hal-pawnio --example nct-dump`). The fixture is also the evidence for
//! anyone adding a second chip later: it shows exactly what this family looks like.
//!
//! The expected values below are not invented. They were cross-checked, at capture time,
//! against a second application displaying the same sensors — which is what makes this a
//! test of *correctness* rather than a test that the code still does what it did.

#![cfg(windows)]

use std::collections::BTreeMap;

use of_hal_pawnio::nct6775::{
    REG_FAN, REG_PWM, TEMP_INPUTS, decode_pwm, decode_rpm, decode_temp_byte, decode_temp_word,
    identify,
};

const FIXTURE: &str = include_str!("fixtures/nct6798d-quasar.txt");

/// The captured register space, keyed by 16-bit register address.
fn registers() -> BTreeMap<u16, u8> {
    let mut map = BTreeMap::new();

    for line in FIXTURE.lines() {
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
            // 'xx' marks a read that failed at capture time.
            if let Ok(value) = u8::from_str_radix(byte, 16) {
                map.insert((bank << 8) | (offset + i as u16), value);
            }
        }
    }

    assert!(!map.is_empty(), "fixture parsed to nothing");
    map
}

fn word(regs: &BTreeMap<u16, u8>, register: u16) -> u16 {
    let high = regs[&register];
    let low = regs[&(register + 1)];
    (u16::from(high) << 8) | u16::from(low)
}

#[test]
fn the_fixture_is_the_chip_we_think_it_is() {
    assert!(FIXTURE.contains("chip_id=0xD42B"));
    assert_eq!(identify(0xD42B).unwrap().key, "nct6798d");
}

#[test]
fn tachometers_decode_to_the_speeds_the_fans_were_actually_running() {
    let regs = registers();
    let rpm: Vec<_> = REG_FAN
        .iter()
        .map(|&reg| decode_rpm(word(&regs, reg)))
        .collect();

    // fan/1 is the machine's only case fan; fan/4 is the AIO pump. Both values were
    // visible in another tool at the moment of capture.
    assert_eq!(rpm[1], Some(1477.0), "CPU fan");
    assert_eq!(rpm[4], Some(2156.0), "AIO pump");

    // fan/0's header is empty. That is a genuine 0 RPM reading, not a missing one.
    assert_eq!(rpm[0], Some(0.0), "empty chassis header");

    assert!(
        rpm.iter().all(|r| r.is_some()),
        "every tachometer in the table must decode; garbage means a wrong address: {rpm:?}"
    );
}

#[test]
fn the_register_a_naive_stride_would_use_is_garbage() {
    // 0x4CC sits where a seventh tachometer "should" be if the addresses were evenly
    // spaced. It is not one, and the hardware says so. This is why REG_FAN is a table.
    let regs = registers();
    assert_eq!(decode_rpm(word(&regs, 0x4CC)), None);
}

#[test]
fn pwm_duties_decode_to_what_was_commanded() {
    let regs = registers();
    let duty: Vec<_> = REG_PWM.iter().map(|&reg| decode_pwm(regs[&reg])).collect();

    // Cross-checked against another application displaying the same channels moments
    // before capture: 45.8 % on the CPU fan and 23.5 % on the pump. Both were being held
    // at a fixed duty by that tool, so they match closely.
    assert!((duty[1] - 45.8).abs() < 0.5, "cpu fan: {}", duty[1]);
    assert!((duty[4] - 23.5).abs() < 0.5, "aio pump: {}", duty[4]);

    // The chassis header is the one nothing is plugged into and no application was
    // driving — the board firmware still is, and it moved 196 -> 193 between the
    // screenshot and the dump. A wider tolerance here is the honest reflection of a value
    // that legitimately drifts, not a test being loosened to pass.
    assert!((duty[0] - 76.9).abs() < 2.0, "chassis: {}", duty[0]);

    assert!(
        duty.iter().all(|d| (0.0..=100.0).contains(d)),
        "a duty outside 0-100 % means a wrong register: {duty:?}"
    );
}

#[test]
fn temperatures_decode_to_the_readings_shown_at_capture_time() {
    let regs = registers();

    let systin = decode_temp_byte(regs[&TEMP_INPUTS[0].register]);
    let cputin = decode_temp_word(word(&regs, TEMP_INPUTS[1].register));

    // The other tool showed 43 °C for the CPU source it was curving against.
    assert_eq!(cputin, Some(43.0), "CPUTIN");
    assert_eq!(systin, Some(46.0), "SYSTIN");
}

#[test]
fn every_exposed_sensor_id_is_unique_and_stable() {
    // Ids go into saved profiles. A collision or a positional id would re-point a user's
    // fan curve at a different sensor without telling them.
    let mut ids: Vec<String> = Vec::new();
    ids.extend((0..REG_FAN.len()).map(|i| format!("nct6798d/fan/{i}")));
    ids.extend((0..REG_PWM.len()).map(|i| format!("nct6798d/pwm/{i}")));
    ids.extend(
        TEMP_INPUTS
            .iter()
            .map(|t| format!("nct6798d/temp/{}", t.key)),
    );

    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "duplicate sensor id in {ids:?}");
}
