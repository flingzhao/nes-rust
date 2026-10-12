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
        close(samples[0], expected as f32);
        clock(&mut bus, 37); // total 75 cycles
        let samples = bus.take_audio_samples();
        assert_eq!(samples.len(), 1);
        close(samples[0], high);
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
