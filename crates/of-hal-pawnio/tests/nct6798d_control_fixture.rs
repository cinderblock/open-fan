//! Control-register tests against a full 16-bank dump from real hardware.
//!
//! Separate from `nct6798d_fixture.rs` because the two captures answer different
//! questions. That one covers banks 0–7 and its expected values were cross-checked against
//! another application's display at capture time, which is what makes it a correctness
//! test. This one covers all 16 banks so the control registers — which live as high as
//! bank 11 — are present at all, and pins what the chip's *control configuration* looked
//! like on a machine in a known state.
//!
//! The known state, on the reference machine at capture time: channel 0's header is empty
//! and still on the BIOS curve, channels 1 and 4 are held in manual by another fan
//! controller, and the rest are untouched.

#![cfg(windows)]

use std::collections::BTreeMap;

use of_hal_pawnio::nct6775::{
    FanMode, REG_FAN_MODE, REG_PWM, REG_PWM_WRITE, decode_pwm, mode_from_register,
    mode_into_register,
};

const FIXTURE: &str = include_str!("fixtures/nct6798d-quasar-control.txt");

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
            if let Ok(value) = u8::from_str_radix(byte, 16) {
                map.insert((bank << 8) | (offset + i as u16), value);
            }
        }
    }

    assert!(!map.is_empty(), "fixture parsed to nothing");
    map
}

#[test]
fn the_capture_reaches_the_banks_control_registers_live_in() {
    // The first capture stopped at bank 7 and silently lacked the write and mode
    // registers for channels 3 to 6 — including the AIO pump's.
    let regs = registers();
    for (index, &register) in REG_PWM_WRITE.iter().enumerate() {
        assert!(
            regs.contains_key(&register),
            "channel {index}'s write register {register:#05X} is not in the capture"
        );
    }
    for &register in &REG_FAN_MODE {
        assert!(regs.contains_key(&register), "{register:#05X} missing");
    }
}

#[test]
fn the_write_register_mirrors_the_readback_register() {
    // The evidence that REG_PWM_WRITE is the right table. If these disagreed, one of the
    // two tables would be addressing something that is not this channel's duty.
    let regs = registers();
    for index in 0..REG_PWM.len() {
        assert_eq!(
            regs[&REG_PWM_WRITE[index]], regs[&REG_PWM[index]],
            "channel {index}: write {:#05X} and readback {:#05X} disagree",
            REG_PWM_WRITE[index], REG_PWM[index]
        );
    }
}

#[test]
fn the_untouched_header_reads_as_firmware_controlled() {
    // Channel 0 had nothing plugged in and no application driving it, so the board
    // firmware still owned it. Smart Fan IV is the BIOS curve. This is the state that
    // makes an acquire/release test meaningful, and the only channel that had it.
    let regs = registers();
    let mode = mode_from_register(regs[&REG_FAN_MODE[0]]);
    assert_eq!(mode, FanMode::SmartFanIv);
    assert!(mode.is_firmware_controlled());
}

#[test]
fn the_channels_another_controller_held_read_as_manual() {
    // Channels 1 (the only case fan) and 4 (the AIO pump) were being driven by another
    // fan controller. They read Manual — which is exactly why acquiring them would
    // capture *that tool's* configuration rather than the firmware's, and why
    // can_restore_firmware_control cannot honestly answer true for them.
    let regs = registers();
    for index in [1, 4] {
        let mode = mode_from_register(regs[&REG_FAN_MODE[index]]);
        assert_eq!(mode, FanMode::Manual, "channel {index}");
        assert!(!mode.is_firmware_controlled(), "channel {index}");
    }
}

#[test]
fn switching_the_firmware_channel_to_manual_changes_only_the_mode() {
    // The precise edit `acquire` makes to the real byte on this machine: 0x40 -> 0x00.
    // The low nibble is the firmware's tolerance setting and must survive.
    let regs = registers();
    let original = regs[&REG_FAN_MODE[0]];
    let manual = mode_into_register(original, FanMode::Manual);

    assert_eq!(mode_from_register(manual), FanMode::Manual);
    assert_eq!(
        manual & 0x0F,
        original & 0x0F,
        "tolerance nibble was not preserved"
    );
    // And the restore is exact.
    assert_eq!(
        mode_into_register(manual, mode_from_register(original)),
        original
    );
}

#[test]
fn every_captured_mode_register_round_trips() {
    // The property release depends on, checked against real bytes rather than only
    // synthetic ones.
    let regs = registers();
    for (index, &register) in REG_FAN_MODE.iter().enumerate() {
        let raw = regs[&register];
        assert_eq!(
            mode_into_register(raw, mode_from_register(raw)),
            raw,
            "channel {index} lost information"
        );
    }
}

#[test]
fn every_captured_duty_decodes_into_range() {
    let regs = registers();
    for (index, &register) in REG_PWM_WRITE.iter().enumerate() {
        let duty = decode_pwm(regs[&register]);
        assert!(
            (0.0..=100.0).contains(&duty),
            "channel {index} decoded to {duty}"
        );
    }
}
