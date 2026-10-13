use std::f64::consts::PI;
use std::f64::consts::TAU; // 2 * pi

pub(crate) const NTSC_CPU_HZ: u32 = 1_789_773;
pub(crate) const AUDIO_SAMPLE_RATE: u32 = 48_000;

pub(crate) struct CpuRateSampler {
    cpu_hz: u32,
    sample_rate: u32,
    filled_units: u32,
    weighted_sum: f64,
}

impl CpuRateSampler {
    pub(crate) fn new(cpu_hz: u32, sample_rate: u32) -> Result<Self, String> {
        if cpu_hz == 0 {
            return Err("cpu hz should be greater than 0".to_string());
        }

        if sample_rate == 0 {
            return Err("sample rate should be greater than 0".to_string());
        }

        if sample_rate > cpu_hz {
            return Err("sample rate should be less than cpu hz".to_string());
        }

        Ok(Self {
            cpu_hz,
            sample_rate,
            filled_units: 0,
            weighted_sum: 0.0,
        })
    }

    // 设 F = cpu_hz 的数值, R = sample_rate 的数值
    // 选择每个刻度代表 1 / (f × r) 秒，就有：
    // 一个 cpu 周期占多少刻度： (1 / f) ÷ (1 / (f × r)) = r
    // 一个输出样本占多少刻度： (1 / r) ÷ (1 / (f × r)) = f
    // cpu 周期的刻度数   = sample_rate 的数值
    // 输出样本的刻度数  = cpu_hz 的数值
    pub(crate) fn push_cycle(&mut self, level: f32) -> Option<f32> {
        let mut output = None;
        let mut remaining_units = self.sample_rate;
        let sample_window_units = self.cpu_hz;

        while remaining_units > 0 {
            let available_units = sample_window_units - self.filled_units;
            let take = remaining_units.min(available_units);

            self.weighted_sum += f64::from(level) * f64::from(take);
            self.filled_units += take;
            remaining_units -= take;

            if self.filled_units == sample_window_units {
                output = Some((self.weighted_sum / f64::from(sample_window_units)) as f32);

                self.filled_units = 0;
                self.weighted_sum = 0.0;
            }
        }

        output
    }
}

struct HighPass {
    coefficient: f64,
    previous_input: f64,
    previous_output: f64,
}

struct LowPass {
    coefficient: f64,
    previous_output: f64,
}

impl HighPass {
    fn new(sample_rate: u32, cutoff_hz: f64) -> Result<Self, String> {
        if sample_rate == 0 {
            return Err("sample_rate should be greater than 0.".to_string());
        }

        if !cutoff_hz.is_finite() || cutoff_hz <= 0.0 || cutoff_hz >= f64::from(sample_rate / 2) {
            return Err("cutoff hz should be between 0 and sample_rate / 2.".to_string());
        }
        let q = TAU * f64::from(cutoff_hz) / f64::from(sample_rate);
        let coefficient = 1.0 / (1.0 + q);
        Ok(Self {
            coefficient,
            previous_input: 0.0,
            previous_output: 0.0,
        })
    }

    fn process(&mut self, input: f64) -> f64 {
        let output = self.coefficient * (self.previous_output + input - self.previous_input);
        self.previous_input = input;
        self.previous_output = output;
        self.previous_output
    }
}

impl LowPass {
    fn new(sample_rate: u32, cutoff_hz: f64) -> Result<Self, String> {
        if sample_rate == 0 {
            return Err("sample_rate should be greater than 0.".to_string());
        }

        if !cutoff_hz.is_finite() || cutoff_hz <= 0.0 || cutoff_hz >= f64::from(sample_rate / 2) {
            return Err("cutoff hz should be between 0 and sample_rate / 2.".to_string());
        }
        let q = TAU * f64::from(cutoff_hz) / f64::from(sample_rate);
        let coefficient = q / (1.0 + q);
        Ok(Self {
            coefficient,
            previous_output: 0.0,
        })
    }

    fn process(&mut self, input: f64) -> f64 {
        let output = self.previous_output + self.coefficient * (input - self.previous_output);
        self.previous_output = output;
        self.previous_output
    }
}

pub(crate) struct AudioFilter {
    high_pass_90: HighPass,
    high_pass_440: HighPass,
    low_pass_14000: LowPass,
}

impl AudioFilter {
    pub(crate) fn new(sample_rate: u32) -> Result<Self, String> {
        Ok(Self {
            high_pass_90: HighPass::new(sample_rate, 90.0)?,
            high_pass_440: HighPass::new(sample_rate, 440.0)?,
            low_pass_14000: LowPass::new(sample_rate, 14_000.0)?,
        })
    }

    pub(crate) fn process(&mut self, input: f32) -> f32 {
        let mut output = self.high_pass_90.process(f64::from(input));
        output = self.high_pass_440.process(output);
        output = self.low_pass_14000.process(output);
        output as f32
    }
}

#[cfg(test)]
mod audio_sampler_tests {
    use super::{AUDIO_SAMPLE_RATE, CpuRateSampler, NTSC_CPU_HZ};
    use crate::{bus::Bus, cartridge::Cartridge, cpu::Cpu, nes::Nes};

    fn close(actual: f32, expected: f32) {
        assert!(actual.is_finite());
        assert!(
            (actual - expected).abs() < 0.000002,
            "actual={actual}, expected={expected}"
        );
    }

    #[test]
    fn invalid_rates_are_rejected() {
        assert!(CpuRateSampler::new(0, 48_000).is_err());
        assert!(CpuRateSampler::new(1_789_773, 0).is_err());
        assert!(CpuRateSampler::new(2, 3).is_err());
    }

    #[test]
    fn equal_rates_return_each_input_unchanged() {
        let mut sampler = CpuRateSampler::new(48_000, 48_000).unwrap();
        for input in [0.0, 0.25, 1.0, -0.25, 0.5] {
            close(sampler.push_cycle(input).unwrap(), input);
        }
    }

    #[test]
    fn integer_ratio_averages_all_cycles_in_each_window() {
        let mut sampler = CpuRateSampler::new(4, 2).unwrap();
        assert_eq!(sampler.push_cycle(0.0), None);
        close(sampler.push_cycle(1.0).unwrap(), 0.5);
        assert_eq!(sampler.push_cycle(0.25), None);
        close(sampler.push_cycle(0.75).unwrap(), 0.5);
    }

    #[test]
    fn fractional_boundary_splits_a_cycle_between_two_samples() {
        let mut sampler = CpuRateSampler::new(5, 2).unwrap();
        assert_eq!(sampler.push_cycle(0.0), None);
        assert_eq!(sampler.push_cycle(0.0), None);
        close(sampler.push_cycle(1.0).unwrap(), 0.2);
        assert_eq!(sampler.push_cycle(1.0), None);
        close(sampler.push_cycle(1.0).unwrap(), 1.0);
    }

    #[test]
    fn fractional_tail_is_kept_even_when_following_cycles_are_zero() {
        let mut sampler = CpuRateSampler::new(5, 2).unwrap();
        let actual: Vec<_> = [0.0, 0.0, 1.0, 0.0, 0.0]
            .into_iter()
            .filter_map(|v| sampler.push_cycle(v))
            .collect();
        assert_eq!(actual.len(), 2);
        close(actual[0], 0.2);
        close(actual[1], 0.2);
    }

    #[test]
    fn constant_level_is_preserved_without_startup_zero_padding() {
        let mut sampler = CpuRateSampler::new(11, 3).unwrap();
        let samples: Vec<_> = (0..110).filter_map(|_| sampler.push_cycle(0.375)).collect();
        assert_eq!(samples.len(), 30);
        for sample in samples {
            close(sample, 0.375);
        }
    }

    #[test]
    fn output_count_matches_integer_time_ratio_at_every_cycle() {
        let mut sampler = CpuRateSampler::new(97, 13).unwrap();
        let mut count = 0u64;
        for elapsed in 1..=9_700u64 {
            count += u64::from(sampler.push_cycle(0.25).is_some());
            assert_eq!(count, elapsed * 13 / 97);
        }
    }

    #[test]
    fn one_model_second_has_exactly_48000_samples() {
        assert_eq!(NTSC_CPU_HZ, 1_789_773);
        assert_eq!(AUDIO_SAMPLE_RATE, 48_000);
        let mut sampler = CpuRateSampler::new(NTSC_CPU_HZ, AUDIO_SAMPLE_RATE).unwrap();
        let mut count = 0;
        for _ in 0..NTSC_CPU_HZ {
            if let Some(sample) = sampler.push_cycle(0.25) {
                close(sample, 0.25);
                count += 1;
            }
        }
        assert_eq!(count, AUDIO_SAMPLE_RATE);
    }

    fn bus_with_program(program: &[u8]) -> Bus {
        let mut rom = vec![0u8; 16 + 0x4000];
        rom[..4].copy_from_slice(b"NES\x1A");
        rom[4] = 1;
        rom[16..16 + program.len()].copy_from_slice(program);
        rom[16 + 0x3FFC..16 + 0x3FFE].copy_from_slice(&0x8000u16.to_le_bytes());
        let mut bus = Bus::new();
        bus.insert_cartridge(Cartridge::try_from(rom.as_slice()).unwrap());
        bus.cpu_write(0x2001, 0).unwrap();
        bus.cpu_write(0x4017, 0x40).unwrap();
        bus
    }

    fn clock(bus: &mut Bus, cycles: usize) {
        for _ in 0..cycles {
            bus.clock_cpu_cycle().unwrap();
        }
    }

    #[test]
    fn draining_bus_samples_does_not_reset_the_unfinished_window() {
        let mut bus = bus_with_program(&[0xEA]);
        assert!(bus.take_audio_samples().is_empty());
        clock(&mut bus, 37);
        assert!(bus.take_audio_samples().is_empty());
        clock(&mut bus, 1);
        assert_eq!(bus.take_audio_samples().len(), 1); // total 38 cycles
        assert!(bus.take_audio_samples().is_empty());
        clock(&mut bus, 36);
        assert!(bus.take_audio_samples().is_empty()); // total 74 cycles
        clock(&mut bus, 1);
        assert_eq!(bus.take_audio_samples().len(), 1); // total 75 cycles
    }

    #[test]
    fn bus_samples_the_mixed_level_and_preserves_fractional_history() {
        let mut filter = super::AudioFilter::new(AUDIO_SAMPLE_RATE).unwrap();
        let mut bus = bus_with_program(&[0xEA]);
        let low = bus.apu().mixed_output();
        clock(&mut bus, 20);
        bus.cpu_write(0x4011, 127).unwrap();
        let high = bus.apu().mixed_output();
        assert!(high > low);
        clock(&mut bus, 18);
        let samples = bus.take_audio_samples();
        assert_eq!(samples.len(), 1);
        let low_units = 20.0 * f64::from(AUDIO_SAMPLE_RATE);
        let expected = (f64::from(low) * low_units
            + f64::from(high) * (f64::from(NTSC_CPU_HZ) - low_units))
            / f64::from(NTSC_CPU_HZ);
        close(samples[0], filter.process(expected as f32));
        clock(&mut bus, 37); // total 75 cycles
        let samples = bus.take_audio_samples();
        assert_eq!(samples.len(), 1);
        close(samples[0], filter.process(high));
    }

    #[test]
    fn oam_dma_stall_cycles_also_generate_audio_samples() {
        let mut bus = bus_with_program(&[0xA9, 0x02, 0x8D, 0x14, 0x40, 0xEA]);
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus).unwrap(); // raw Bus reset does not advance clocks
        let mut nes = Nes::new(cpu, bus);
        assert!(nes.take_audio_samples().is_empty());
        assert_eq!(nes.step().unwrap(), 2);
        assert_eq!(nes.step().unwrap(), 4);
        assert_eq!(nes.cpu_cycles(), 6);
        assert!(nes.take_audio_samples().is_empty());
        assert_eq!(nes.step().unwrap(), 2);
        assert_eq!(nes.cpu_cycles(), 521);
        let samples = nes.take_audio_samples();
        assert_eq!(samples.len(), 13);
        assert!(samples.iter().all(|v| v.is_finite()));
        assert!(nes.take_audio_samples().is_empty());
    }
}

#[cfg(test)]
mod audio_filter_tests {
    use super::{AUDIO_SAMPLE_RATE, AudioFilter, CpuRateSampler, HighPass, LowPass, NTSC_CPU_HZ};
    use crate::{bus::Bus, cartridge::Cartridge};

    fn close(actual: f64, expected: f64, tolerance: f64) {
        assert!(actual.is_finite());
        assert!(
            (actual - expected).abs() < tolerance,
            "actual={actual}, expected={expected}, tolerance={tolerance}"
        );
    }

    #[test]
    fn rejects_invalid_filter_parameters() {
        assert!(HighPass::new(0, 90.0).is_err());
        assert!(LowPass::new(0, 90.0).is_err());
        for cutoff in [
            0.0,
            -1.0,
            24_000.0,
            30_000.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            assert!(HighPass::new(48_000, cutoff).is_err());
            assert!(LowPass::new(48_000, cutoff).is_err());
        }
        for rate in [0, 8_000, 28_000] {
            assert!(AudioFilter::new(rate).is_err());
        }
        assert!(AudioFilter::new(44_100).is_ok());
        assert!(AudioFilter::new(48_000).is_ok());
    }

    #[test]
    fn high_pass_remembers_both_previous_input_and_output() {
        let mut filter = HighPass::new(48_000, 90.0).unwrap();
        let a = 1.0 / (1.0 + std::f64::consts::TAU * 90.0 / 48_000.0);
        close(filter.process(1.0), a, 1e-12);
        close(filter.process(1.0), a * a, 1e-12);
        close(filter.process(0.0), a * (a * a - 1.0), 1e-12);
    }

    #[test]
    fn low_pass_moves_toward_input_without_immediately_replacing_output() {
        let mut filter = LowPass::new(48_000, 14_000.0).unwrap();
        let q = std::f64::consts::TAU * 14_000.0 / 48_000.0;
        let b = q / (1.0 + q);
        let first = b;
        let second = first + b * (1.0 - first);
        close(filter.process(1.0), first, 1e-12);
        close(filter.process(1.0), second, 1e-12);
        close(filter.process(0.0), second * (1.0 - b), 1e-12);
    }

    #[test]
    fn zero_input_stays_zero() {
        let mut filter = AudioFilter::new(48_000).unwrap();
        for _ in 0..1000 {
            assert_eq!(filter.process(0.0), 0.0);
        }
    }

    #[test]
    fn cascade_matches_known_startup_values() {
        let mut filter = AudioFilter::new(48_000).unwrap();
        for expected in [
            0.30230534446255963,
            0.38904583150381944,
            0.40081405349886706,
        ] {
            close(f64::from(filter.process(0.5)), expected, 1e-6);
        }
    }

    #[test]
    fn constant_input_decays_toward_zero() {
        let mut filter = AudioFilter::new(48_000).unwrap();
        let mut output = 0.0;
        for _ in 0..4800 {
            output = filter.process(0.5);
        }
        assert!(output.abs() < 1e-6, "output={output}");
    }

    #[test]
    fn negative_values_are_preserved_and_not_clamped() {
        let mut positive = AudioFilter::new(48_000).unwrap();
        let mut negative = AudioFilter::new(48_000).unwrap();
        for i in 0..1000 {
            let x = if i < 100 { 0.5 } else { 0.0 };
            close(
                f64::from(negative.process(-x)),
                -f64::from(positive.process(x)),
                1e-6,
            );
        }
        let mut filter = AudioFilter::new(48_000).unwrap();
        assert!(filter.process(-0.5) < 0.0);
    }

    #[test]
    fn high_pass_retains_changes_while_low_pass_reduces_fast_changes() {
        let mut high = HighPass::new(48_000, 90.0).unwrap();
        let mut low = LowPass::new(48_000, 14_000.0).unwrap();
        let mut high_output = 0.0;
        let mut low_output = 0.0;
        for i in 0..4800 {
            let input = if i % 2 == 0 { 1.0 } else { -1.0 };
            high_output = high.process(input);
            low_output = low.process(input);
        }
        assert!(high_output.abs() > 0.9);
        assert!(low_output.abs() < 0.6);
    }

    fn bus_with_cartridge() -> Bus {
        let mut rom = vec![0u8; 16 + 0x4000];
        rom[..4].copy_from_slice(b"NES\x1A");
        rom[4] = 1;
        let mut bus = Bus::new();
        bus.insert_cartridge(Cartridge::try_from(rom.as_slice()).unwrap());
        bus.cpu_write(0x2001, 0).unwrap();
        bus.cpu_write(0x4017, 0x40).unwrap();
        bus
    }

    #[test]
    fn bus_filters_once_per_sample_and_draining_preserves_all_history() {
        let mut bus = bus_with_cartridge();
        let mut sampler = CpuRateSampler::new(NTSC_CPU_HZ, AUDIO_SAMPLE_RATE).unwrap();
        let mut filter = AudioFilter::new(AUDIO_SAMPLE_RATE).unwrap();
        let mut expected = Vec::new();
        let mut actual = Vec::new();
        for cycle in 0..10_000u32 {
            if cycle % 113 == 0 {
                bus.cpu_write(0x4011, ((cycle / 113) % 128) as u8).unwrap();
            }
            bus.clock_cpu_cycle().unwrap();
            let level = bus.apu().mixed_output();
            if let Some(sample) = sampler.push_cycle(level) {
                expected.push(filter.process(sample));
            }
            if cycle % 37 == 0 {
                actual.extend(bus.take_audio_samples());
            }
        }
        actual.extend(bus.take_audio_samples());
        assert_eq!(
            actual.len(),
            (10_000u64 * u64::from(AUDIO_SAMPLE_RATE) / u64::from(NTSC_CPU_HZ)) as usize
        );
        assert_eq!(actual.len(), expected.len());
        for (a, e) in actual.into_iter().zip(expected) {
            close(f64::from(a), f64::from(e), 1e-6);
        }
        assert!(bus.take_audio_samples().is_empty());
    }
}
