//! A simulated thermal plant implementing the OpenFan HAL.
//!
//! This is not a stub that returns constants. It is a first-order lumped thermal model
//! with real dynamics — heat capacity, a fan-speed-dependent cooling coefficient, and a
//! transport delay between commanding a duty and the temperature responding.
//!
//! That transport delay is the point. It is what makes an over-aggressive curve hunt,
//! and hunting is the behaviour the limit-cycle detection planned for Phase 8 has to
//! find. A model that responds instantly could never reproduce the bug we are trying to
//! catch. It also means the control loop can be exercised in CI, on a machine with no
//! fans at all, against dynamics that behave qualitatively like a real heatsink.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, VecDeque};

use of_hal::{
    Backend, ChannelId, ChannelInfo, HalError, OutputChannel, Result, SensorId, SensorInfo,
    SensorKind, SensorSource,
};

/// Tunables for the simulated plant. Defaults are loosely desktop-CPU shaped.
#[derive(Debug, Clone)]
pub struct PlantConfig {
    /// Ambient temperature the plant decays towards, °C.
    pub ambient_c: f64,
    /// Heat injected by the load, in °C/s at 100 % load with no cooling.
    pub heat_rate_c_per_s: f64,
    /// Cooling authority at 100 % duty, as a fraction of (temp - ambient) removed per second.
    pub cooling_coeff: f64,
    /// Cooling still present at 0 % duty — natural convection. Without this a stopped fan
    /// means infinite temperature, which is unrealistic and hides real bugs.
    pub passive_coeff: f64,
    /// Ticks between commanding a duty and the plant feeling it.
    pub transport_delay_ticks: usize,
    /// Fan speed at 100 % duty.
    pub max_rpm: f64,
    /// Duty below which the fan stalls and reads 0 RPM.
    pub stall_duty: f64,
    /// Seconds per tick, used to scale the thermal integration.
    pub tick_seconds: f64,
}

impl Default for PlantConfig {
    fn default() -> Self {
        Self {
            ambient_c: 22.0,
            heat_rate_c_per_s: 9.0,
            cooling_coeff: 0.35,
            passive_coeff: 0.02,
            transport_delay_ticks: 8,
            max_rpm: 2000.0,
            stall_duty: 12.0,
            tick_seconds: 0.1,
        }
    }
}

/// A simulated machine: one heat source, one fan, one temperature sensor.
#[derive(Debug)]
pub struct MockBackend {
    config: PlantConfig,
    /// Current die temperature.
    temp_c: f64,
    /// Load fraction 0..=1, driven by the test or demo.
    load: f64,
    /// Commanded duty, pre-delay.
    commanded_duty: f64,
    /// Duties in flight through the transport delay.
    pipeline: VecDeque<f64>,
    /// Duty currently affecting the plant.
    effective_duty: f64,
    acquired: bool,
    /// Whether `release` was called — lets tests assert the dying breath actually ran.
    pub released: bool,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new(PlantConfig::default())
    }
}

impl MockBackend {
    pub const TEMP_SENSOR: &'static str = "mock/temp/cpu";
    pub const RPM_SENSOR: &'static str = "mock/fan/1";
    pub const CHANNEL: &'static str = "mock/pwm/1";

    pub fn new(config: PlantConfig) -> Self {
        let pipeline = VecDeque::from(vec![0.0; config.transport_delay_ticks]);
        Self {
            temp_c: config.ambient_c,
            config,
            load: 0.0,
            commanded_duty: 0.0,
            pipeline,
            effective_duty: 0.0,
            acquired: false,
            released: false,
        }
    }

    /// Set the simulated workload, 0.0..=1.0.
    pub fn set_load(&mut self, load: f64) {
        self.load = load.clamp(0.0, 1.0);
    }

    pub fn temperature(&self) -> f64 {
        self.temp_c
    }

    pub fn effective_duty(&self) -> f64 {
        self.effective_duty
    }

    /// Advance the simulation by one tick.
    ///
    /// Call this once per control tick, after the engine has written its duties.
    pub fn step(&mut self) {
        // Push the newly commanded duty into the delay line and pull out what the plant
        // actually feels this tick.
        self.pipeline.push_back(self.commanded_duty);
        self.effective_duty = self.pipeline.pop_front().unwrap_or(0.0);

        let dt = self.config.tick_seconds;
        let above_ambient = self.temp_c - self.config.ambient_c;

        // A stalled fan provides no forced convection regardless of commanded duty —
        // the case that makes "quiet" and "not cooling" look identical from the outside.
        let airflow =
            if self.effective_duty < self.config.stall_duty { 0.0 } else { self.effective_duty / 100.0 };

        let heating = self.config.heat_rate_c_per_s * self.load;
        let cooling =
            (self.config.passive_coeff + self.config.cooling_coeff * airflow) * above_ambient;

        self.temp_c += (heating - cooling) * dt;
        if self.temp_c < self.config.ambient_c {
            self.temp_c = self.config.ambient_c;
        }
    }

    fn rpm(&self) -> f64 {
        if self.effective_duty < self.config.stall_duty {
            0.0
        } else {
            self.config.max_rpm * (self.effective_duty / 100.0)
        }
    }
}

impl SensorSource for MockBackend {
    fn sensors(&self) -> Result<Vec<SensorInfo>> {
        Ok(vec![
            SensorInfo {
                id: Self::TEMP_SENSOR.into(),
                label: "Simulated CPU".into(),
                kind: SensorKind::Temperature,
            },
            SensorInfo {
                id: Self::RPM_SENSOR.into(),
                label: "Simulated fan".into(),
                kind: SensorKind::Fan,
            },
        ])
    }

    fn read_all(&mut self) -> Result<BTreeMap<SensorId, f64>> {
        Ok(BTreeMap::from([
            (Self::TEMP_SENSOR.to_owned(), self.temp_c),
            (Self::RPM_SENSOR.to_owned(), self.rpm()),
        ]))
    }
}

impl OutputChannel for MockBackend {
    fn channels(&self) -> Result<Vec<ChannelInfo>> {
        Ok(vec![ChannelInfo {
            id: Self::CHANNEL.into(),
            label: "Simulated fan header".into(),
            tachometer: Some(Self::RPM_SENSOR.into()),
            min_reliable_duty: Some(self.config.stall_duty),
        }])
    }

    fn acquire(&mut self, channel: &ChannelId) -> Result<()> {
        if channel != Self::CHANNEL {
            return Err(HalError::UnknownChannel(channel.clone()));
        }
        self.acquired = true;
        self.released = false;
        Ok(())
    }

    fn set_duty(&mut self, channel: &ChannelId, percent: f64) -> Result<()> {
        if channel != Self::CHANNEL {
            return Err(HalError::UnknownChannel(channel.clone()));
        }
        if !self.acquired {
            return Err(HalError::NotAcquired(channel.clone()));
        }
        self.commanded_duty = percent.clamp(0.0, 100.0);
        Ok(())
    }

    fn release(&mut self, channel: &ChannelId) -> Result<()> {
        if channel != Self::CHANNEL {
            return Err(HalError::UnknownChannel(channel.clone()));
        }
        self.acquired = false;
        self.released = true;
        Ok(())
    }

    fn can_restore_firmware_control(&self) -> bool {
        true
    }
}

impl Backend for MockBackend {
    fn name(&self) -> String {
        "Simulated thermal plant".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settle(plant: &mut MockBackend, duty: f64, ticks: usize) {
        plant.acquire(&MockBackend::CHANNEL.to_owned()).unwrap();
        plant.set_duty(&MockBackend::CHANNEL.to_owned(), duty).unwrap();
        for _ in 0..ticks {
            plant.step();
        }
    }

    #[test]
    fn load_heats_the_plant_and_cooling_holds_it_down() {
        let mut hot = MockBackend::default();
        hot.set_load(1.0);
        settle(&mut hot, 0.0, 600);

        let mut cooled = MockBackend::default();
        cooled.set_load(1.0);
        settle(&mut cooled, 100.0, 600);

        assert!(hot.temperature() > cooled.temperature() + 20.0,
            "full duty must be clearly cooler: {} vs {}", hot.temperature(), cooled.temperature());
    }

    #[test]
    fn an_idle_plant_decays_to_ambient() {
        let mut plant = MockBackend::default();
        plant.set_load(1.0);
        settle(&mut plant, 50.0, 300);
        assert!(plant.temperature() > PlantConfig::default().ambient_c + 5.0);

        plant.set_load(0.0);
        for _ in 0..3000 {
            plant.step();
        }
        assert!((plant.temperature() - PlantConfig::default().ambient_c).abs() < 1.0);
    }

    #[test]
    fn a_stalled_fan_reads_zero_rpm_and_provides_no_cooling() {
        let cfg = PlantConfig::default();
        let mut plant = MockBackend::new(cfg.clone());
        plant.set_load(1.0);
        // Just below the stall threshold: commanded, but not actually moving air.
        settle(&mut plant, cfg.stall_duty - 1.0, 400);

        assert_eq!(plant.read_all().unwrap()[MockBackend::RPM_SENSOR], 0.0);

        let mut off = MockBackend::new(cfg);
        off.set_load(1.0);
        settle(&mut off, 0.0, 400);
        assert!((plant.temperature() - off.temperature()).abs() < 0.5,
            "a stalled fan must cool no better than a stopped one");
    }

    #[test]
    fn commanded_duty_takes_the_transport_delay_to_reach_the_plant() {
        let cfg = PlantConfig { transport_delay_ticks: 5, ..Default::default() };
        let mut plant = MockBackend::new(cfg);
        plant.acquire(&MockBackend::CHANNEL.to_owned()).unwrap();
        plant.set_duty(&MockBackend::CHANNEL.to_owned(), 100.0).unwrap();

        for _ in 0..5 {
            plant.step();
            assert_eq!(plant.effective_duty(), 0.0, "duty must not arrive early");
        }
        plant.step();
        assert_eq!(plant.effective_duty(), 100.0);
    }

    #[test]
    fn driving_an_unacquired_channel_is_refused() {
        let mut plant = MockBackend::default();
        let err = plant.set_duty(&MockBackend::CHANNEL.to_owned(), 50.0).unwrap_err();
        assert!(matches!(err, HalError::NotAcquired(_)));
    }

    #[test]
    fn release_is_observable() {
        let mut plant = MockBackend::default();
        plant.acquire(&MockBackend::CHANNEL.to_owned()).unwrap();
        assert!(!plant.released);
        plant.release(&MockBackend::CHANNEL.to_owned()).unwrap();
        assert!(plant.released, "tests need to see that the dying breath ran");
    }
}
