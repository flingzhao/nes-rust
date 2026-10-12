#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ChannelLevels {
    pub pulse1: u8,
    pub pulse2: u8,
    pub triangle: u8,
    pub noise: u8,
    pub dmc: u8,
}

pub(crate) fn mix(levels: ChannelLevels) -> f32 {
    debug_assert!((0..=15).contains(&levels.pulse1));
    debug_assert!((0..=15).contains(&levels.pulse2));
    debug_assert!((0..=15).contains(&levels.triangle));
    debug_assert!((0..=15).contains(&levels.noise));
    debug_assert!((0..=127).contains(&levels.dmc));

    let pulse_sum = levels.pulse1 + levels.pulse2;
    let pulse_out = if pulse_sum == 0 {
        0.0
    } else {
        95.88 / (8128.0 / f32::from(pulse_sum) + 100.0)
    };

    let tnd_input = f32::from(levels.triangle) / 8227.0
        + f32::from(levels.noise) / 12241.0
        + f32::from(levels.dmc) / 22638.0;

    let tnd_out = if tnd_input == 0f32 {
        0f32
    } else {
        159.79 / (1.0 / tnd_input + 100.0)
    };

    let mixed = pulse_out + tnd_out;
    mixed
}

#[cfg(test)]
mod mixer_tests {
    use super::{ChannelLevels, mix};
    use crate::bus::Bus;

    fn levels(values: [u8; 5]) -> ChannelLevels {
        ChannelLevels {
            pulse1: values[0],
            pulse2: values[1],
            triangle: values[2],
            noise: values[3],
            dmc: values[4],
        }
    }

    fn assert_close(actual: f32, expected: f64) {
        assert!(actual.is_finite(), "non-finite mixed level: {actual}");
        assert!(
            (f64::from(actual) - expected).abs() < 0.000002,
            "actual={actual:.9}, expected={expected:.9}"
        );
    }

    #[test]
    fn all_zero_inputs_produce_exact_zero() {
        assert_eq!(mix(ChannelLevels::default()), 0.0);
    }

    #[test]
    fn individual_channels_match_reference_levels() {
        for (input, expected) in [
            ([15, 0, 0, 0, 0], 0.14937681761528873),
            ([0, 15, 0, 0, 0], 0.14937681761528873),
            ([0, 0, 15, 0, 0], 0.24641204893595145),
            ([0, 0, 0, 15, 0], 0.17443053635106615),
            ([0, 0, 0, 0, 127], 0.574263682155187),
            ([0, 0, 0, 0, 64], 0.3521785246917832),
        ] {
            assert_close(mix(levels(input)), expected);
        }
    }

    #[test]
    fn simultaneous_channels_match_reference_levels() {
        assert_close(mix(levels([15, 15, 0, 0, 0])), 0.25848310567936733);
        assert_close(mix(levels([6, 9, 7, 4, 64])), 0.6063053631313268);
        assert_close(mix(levels([15, 15, 15, 15, 127])), 0.9999993508269456);
    }

    #[test]
    fn pulse_channels_are_symmetric_but_not_linearly_additive() {
        for p1 in 0..=15 {
            for p2 in 0..=15 {
                assert_eq!(
                    mix(levels([p1, p2, 3, 5, 64])),
                    mix(levels([p2, p1, 3, 5, 64]))
                );
            }
        }
        let one = mix(levels([15, 0, 0, 0, 0]));
        let both = mix(levels([15, 15, 0, 0, 0]));
        assert!(both > one);
        assert!(both < 2.0 * one);
    }

    #[test]
    fn pulse_and_tnd_groups_are_added_after_their_own_mixing() {
        let pulse = mix(levels([6, 9, 0, 0, 0]));
        let tnd = mix(levels([0, 0, 7, 4, 64]));
        assert_close(mix(levels([6, 9, 7, 4, 64])), f64::from(pulse + tnd));
    }

    #[test]
    fn held_dmc_level_changes_triangle_swing() {
        let low_without_dmc = mix(levels([0, 0, 0, 0, 0]));
        let high_without_dmc = mix(levels([0, 0, 15, 0, 0]));
        let low_with_dmc = mix(levels([0, 0, 0, 0, 64]));
        let high_with_dmc = mix(levels([0, 0, 15, 0, 64]));
        assert_close(low_with_dmc, 0.3521785246917832);
        assert_close(high_with_dmc, 0.507210802581757);
        assert_close(high_without_dmc - low_without_dmc, 0.24641204893595145);
        assert_close(high_with_dmc - low_with_dmc, 0.1550322778899738);
        assert!(high_with_dmc - low_with_dmc < high_without_dmc - low_without_dmc);
    }

    #[test]
    fn representative_legal_inputs_are_finite_and_in_range() {
        for p1 in [0, 7, 15] {
            for p2 in [0, 7, 15] {
                for triangle in [0, 7, 15] {
                    for noise in [0, 7, 15] {
                        for dmc in [0, 64, 127] {
                            let value = mix(levels([p1, p2, triangle, noise, dmc]));
                            assert!(value.is_finite());
                            assert!((0.0..=1.000001).contains(&value));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn each_tnd_input_increases_level_without_integer_division_truncation() {
        for channel in 2..=4 {
            let maximum = if channel == 4 { 127 } else { 15 };
            let mut input = [0u8; 5];
            let mut previous = mix(levels(input));
            for value in 1..=maximum {
                input[channel] = value;
                let current = mix(levels(input));
                assert!(current > previous, "channel={channel}, value={value}");
                previous = current;
            }
        }
    }

    #[test]
    fn apu_forwards_actual_channel_outputs_without_advancing_them() {
        let mut bus = Bus::new();
        bus.cpu_write(0x4011, 64).unwrap();
        let apu = bus.apu();
        let expected = ChannelLevels {
            pulse1: apu.pulses[0].output(),
            pulse2: apu.pulses[1].output(),
            triangle: apu.triangle.output(),
            noise: apu.noise.output(),
            dmc: apu.dmc.output(),
        };
        assert_eq!(apu.raw_levels(), expected);
        for _ in 0..20 {
            assert_eq!(apu.mixed_output(), mix(expected));
            assert_eq!(apu.raw_levels(), expected);
        }
    }

    #[test]
    fn disabling_dmc_preserves_its_dac_contribution() {
        let mut bus = Bus::new();
        bus.cpu_write(0x4011, 64).unwrap();
        bus.cpu_write(0x4015, 0x10).unwrap();
        assert!(bus.dmc_active());
        let before_levels = bus.apu().raw_levels();
        let before_mix = bus.apu().mixed_output();

        bus.cpu_write(0x4015, 0).unwrap();
        assert!(!bus.dmc_active());
        let after_levels = bus.apu().raw_levels();
        assert_eq!(before_levels.dmc, 64);
        assert_eq!(after_levels.dmc, 64);
        assert_eq!(after_levels, before_levels);
        assert_eq!(bus.apu().mixed_output(), before_mix);

        let without_dmc = ChannelLevels {
            dmc: 0,
            ..after_levels
        };
        assert!(bus.apu().mixed_output() > mix(without_dmc));
    }
}
