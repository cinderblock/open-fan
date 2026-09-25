//! Every temperature the chip generates, read at a fixed address, checked against two
//! captures from the reference board taken four days apart.
//!
//! Two captures matter more than one here. A register that holds the same byte in both is
//! probably configuration; one that moved is a reading. And a reading that moved *in step
//! with another register* is evidence the two describe the same thing — which is how the
//! source-28 register was tied to the CPU fan monitors without writing anything.

#![cfg(windows)]

use std::collections::BTreeMap;

use of_hal_pawnio::nct6775::{
    REG_TEMP_MON, REG_TEMP_SEL, TEMP_INPUTS, TempEncoding, decode_smart_fan_curve, decode_temp,
    decode_temp_word, decode_tsi_temp, temp_input_for_source,
};

const OLD: &str = include_str!("fixtures/nct6798d-quasar.txt");
const NEW: &str = include_str!("fixtures/nct6798d-quasar-full.txt");

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

fn word(regs: &BTreeMap<u16, u8>, register: u16) -> u16 {
    (u16::from(regs[&register]) << 8) | u16::from(regs[&(register + 1)])
}

/// Read an input exactly as the backend would, from a captured register map.
fn read(regs: &BTreeMap<u16, u8>, input: &of_hal_pawnio::nct6775::TempInput) -> Option<f64> {
    let raw = match input.encoding {
        TempEncoding::SignedByte => u16::from(regs[&input.register]),
        TempEncoding::SignedWord | TempEncoding::Tsi => word(regs, input.register),
    };
    decode_temp(input.encoding, raw)
}

fn input(key: &str) -> &'static of_hal_pawnio::nct6775::TempInput {
    TEMP_INPUTS
        .iter()
        .find(|t| t.key == key)
        .unwrap_or_else(|| panic!("no input keyed {key}"))
}

#[test]
fn every_input_decodes_to_a_plausible_temperature_in_both_captures() {
    for (name, fixture) in [("old", OLD), ("new", NEW)] {
        let regs = registers(fixture);
        for t in &TEMP_INPUTS {
            let value = read(&regs, t);
            assert!(
                value.is_some(),
                "{name}: {} at {:#05x} did not decode — wrong register or wrong encoding",
                t.key,
                t.register
            );
        }
    }
}

#[test]
fn systin_and_cputin_still_read_what_the_other_tool_showed_at_capture() {
    // The values pinned when the first capture was cross-checked against a second
    // application: 46 °C SYSTIN, 43 °C CPUTIN. Moving these two inputs from slot
    // registers to fixed source registers must not change what they say.
    let regs = registers(OLD);
    assert_eq!(read(&regs, input("systin")), Some(46.0));
    assert_eq!(read(&regs, input("cputin")), Some(43.0));
}

#[test]
fn the_source_28_register_moves_in_step_with_every_fan_monitor_that_follows_it() {
    // The tie that makes source 28 readable without a write. Each fan channel has a
    // half-degree monitor showing whatever its REG_TEMP_SEL names. On this board channels
    // 0, 1 and 4 name source 28. If 0x4F4 really holds source 28, it must equal the whole-
    // degree part of those monitors — in both captures, at two different temperatures.
    for (name, fixture) in [("old", OLD), ("new", NEW)] {
        let regs = registers(fixture);
        let peci = read(&regs, input("peci0cal")).expect("source 28 reads");

        let mut checked = 0;
        for ch in 0..6 {
            // The first capture stops at bank 7, so channels 3-5 are simply absent there.
            let Some(sel) = regs.get(&REG_TEMP_SEL[ch]).map(|b| b & 0x1F) else {
                continue;
            };
            if sel != 28 {
                continue;
            }
            let monitor = decode_temp_word(word(&regs, REG_TEMP_MON[ch])).expect("monitor reads");
            assert_eq!(
                monitor.floor(),
                peci,
                "{name}: channel {ch} follows source 28 and shows {monitor}, but 0x4F4 says {peci}"
            );
            checked += 1;
        }
        assert!(
            checked >= 2,
            "{name}: expected several channels on source 28, found {checked}"
        );
    }
}

#[test]
fn the_source_3_register_holds_a_value_while_no_slot_displays_source_3() {
    // The proof that these registers are select-independent: AUXTIN0 (source 3) reads a
    // live temperature from 0x492 even though every monitored slot selects 0, 1, 2 or
    // Virtual. A slot-based read would have had nothing to look at.
    let regs = registers(NEW);
    let slot_selects: Vec<u8> = (0x621..=0x626).map(|r| regs[&r] & 0x1F).collect();
    assert!(
        !slot_selects.contains(&3),
        "a slot shows source 3; the premise is stale"
    );

    let auxtin0 = read(&regs, input("auxtin0")).expect("reads anyway");
    // And it agrees with the monitors of the channels that follow source 3.
    for ch in 0..6 {
        if regs.get(&REG_TEMP_SEL[ch]).map(|b| b & 0x1F) == Some(3) {
            let monitor = decode_temp_word(word(&regs, REG_TEMP_MON[ch])).unwrap();
            assert_eq!(monitor.floor(), auxtin0, "channel {ch}");
        }
    }
}

#[test]
fn source_28_is_a_reading_not_configuration() {
    // Between captures it moved 46 -> 62 while the machine went from idle to building.
    // Configuration does not do that.
    assert_eq!(read(&registers(OLD), input("peci0cal")), Some(46.0));
    assert_eq!(read(&registers(NEW), input("peci0cal")), Some(62.0));
}

#[test]
fn tsi_decodes_the_way_the_driver_does_and_zero_means_absent() {
    // (raw >> 5) * 0.125, per tsi_temp_from_reg. Values read off the captures by hand.
    assert_eq!(decode_tsi_temp(0x4960), Some(73.375));
    assert_eq!(decode_tsi_temp(0x2C00), Some(44.0));
    assert_eq!(decode_tsi_temp(0x38C0), Some(56.75));
    assert_eq!(
        decode_tsi_temp(0),
        None,
        "zero is the driver's absent marker"
    );

    let regs = registers(NEW);
    assert_eq!(read(&regs, input("tsi0")), Some(73.375));
    assert_eq!(read(&regs, input("tsi1")), Some(44.0));
}

#[test]
fn a_chip_curve_can_name_the_input_it_follows() {
    // The link offload needs: from a channel's curve to a graph sensor id, and back.
    let regs = registers(NEW);
    let curve = decode_smart_fan_curve(0, |r| regs.get(&r).copied()).expect("channel 0");
    let followed = curve.followed_input().expect("source 28 is exposed now");
    assert_eq!(followed.key, "peci0cal");

    let curve2 = decode_smart_fan_curve(2, |r| regs.get(&r).copied()).expect("channel 2");
    assert_eq!(curve2.followed_input().map(|t| t.key), Some("auxtin0"));

    // A source we do not expose resolves to nothing rather than to a neighbour.
    assert!(temp_input_for_source(20).is_none());
    // The high three bits of a select byte are not part of the source.
    assert_eq!(
        temp_input_for_source(0b1110_0010).map(|t| t.key),
        Some("cputin")
    );
}

#[test]
fn every_exposed_input_has_a_unique_stable_key_and_register() {
    let mut keys = std::collections::BTreeSet::new();
    let mut regs = std::collections::BTreeSet::new();
    for t in &TEMP_INPUTS {
        assert!(keys.insert(t.key), "duplicate key {}", t.key);
        assert!(
            regs.insert(t.register),
            "duplicate register {:#05x}",
            t.register
        );
        assert!(
            t.key
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "{} is not a stable key",
            t.key
        );
    }
}
