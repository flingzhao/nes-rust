use nes::apu::pulse::PulseOscillator;

fn clock(pulse: &mut PulseOscillator, cycles: usize) {
    for _ in 0..cycles {
        pulse.clock_cpu_cycle();
    }
}

#[test]
fn rejects_invalid_parameters() {
    assert!(PulseOscillator::new(4, 8, 10).is_err());
    assert!(PulseOscillator::new(0, 0x0800, 10).is_err());
    assert!(PulseOscillator::new(0, 8, 16).is_err());

    assert!(PulseOscillator::new(3, 0x07FF, 15).is_ok());
    assert!(PulseOscillator::new(0, 0, 0).is_ok());
}

#[test]
fn timer_waits_for_period_plus_one_half_rate_ticks() {
    let mut pulse = PulseOscillator::new(0, 8, 10).unwrap();

    // 初始处于序列位置 0，输出为 0。
    assert_eq!(pulse.output(), 0);

    // 每两个 CPU 周期推进定时器一次。
    // 第 16 个周期只是把 timer 从 1 减到 0。
    clock(&mut pulse, 17);
    assert_eq!(pulse.output(), 0);

    // 第 18 个周期：重装 timer，序列从位置 0 前进到 1。
    clock(&mut pulse, 1);
    assert_eq!(pulse.output(), 10);

    clock(&mut pulse, 18);
    assert_eq!(pulse.output(), 0);
}

#[test]
fn produces_all_four_duty_sequences_and_wraps() {
    let expected = [
        [0, 10, 0, 0, 0, 0, 0, 0],
        [0, 10, 10, 0, 0, 0, 0, 0],
        [0, 10, 10, 10, 10, 0, 0, 0],
        [10, 0, 0, 10, 10, 10, 10, 10],
    ];

    for (duty, sequence) in expected.iter().enumerate() {
        let mut pulse = PulseOscillator::new(duty as u8, 8, 10).unwrap();

        // 连续检查两轮，覆盖位置 7 -> 0 的回绕。
        for repetition in 0..2 {
            for (step, &output) in sequence.iter().enumerate() {
                assert_eq!(
                    pulse.output(),
                    output,
                    "duty={duty}, repetition={repetition}, step={step}"
                );
                clock(&mut pulse, 18);
            }
        }
    }
}

#[test]
fn volume_controls_high_level() {
    for volume in 0u8..=15 {
        let mut pulse = PulseOscillator::new(0, 8, volume).unwrap();

        assert_eq!(pulse.output(), 0);
        clock(&mut pulse, 18);
        assert_eq!(pulse.output(), volume);
    }
}

#[test]
fn periods_below_eight_are_muted() {
    for period in 0u16..8 {
        let mut pulse = PulseOscillator::new(3, period, 15).unwrap();

        for _ in 0..256 {
            assert_eq!(pulse.output(), 0, "period={period}");
            pulse.clock_cpu_cycle();
        }
    }
}

#[test]
fn period_253_repeats_after_4064_cpu_cycles() {
    let mut pulse = PulseOscillator::new(0, 253, 10).unwrap();

    // 一个位置持续 2 * (253 + 1) = 508 个 CPU 周期。
    clock(&mut pulse, 507);
    assert_eq!(pulse.output(), 0);

    clock(&mut pulse, 1);
    assert_eq!(pulse.output(), 10);

    // 一个完整波形持续 508 * 8 = 4064 个 CPU 周期。
    clock(&mut pulse, 4064);
    assert_eq!(pulse.output(), 10);

    // 再前进一步，位置 1 -> 2，输出恢复为 0。
    clock(&mut pulse, 508);
    assert_eq!(pulse.output(), 0);
}
