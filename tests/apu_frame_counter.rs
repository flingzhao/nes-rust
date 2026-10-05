use nes::apu::Apu;

fn advance(apu: &mut Apu, cycles: usize) {
    for _ in 0..cycles {
        apu.clock_cpu_cycle();
    }
}

fn configure_pulse(apu: &mut Apu, base: u16, control: u8, length_register: u8) {
    apu.write_register(base, control);
    apu.write_register(base + 2, 8);
    apu.write_register(base + 3, length_register);
}

#[test]
fn envelopes_advance_automatically_but_constant_volume_stays_fixed() {
    let mut apu = Apu::new();
    apu.write_register(0x4015, 3);

    // 两个声道均使用 duty=3、timer_period=8。
    // Pulse 1：包络参数 0。
    // Pulse 2：固定音量 7。
    // 长度表索引 1 对应 254，避免检查期间长度归零。
    configure_pulse(&mut apu, 0x4000, 0xC0, 0x08);
    configure_pulse(&mut apu, 0x4004, 0xD7, 0x08);

    // 按当前振荡器的初始相位，这些检查点均处于高电平。
    advance(&mut apu, 7_456);
    assert_eq!(apu.pulse_outputs(), [0, 7]);

    advance(&mut apu, 1); // 总周期 7457：第一次 quarter
    assert_eq!(apu.pulse_outputs(), [15, 7]);

    advance(&mut apu, 7_455); // 总周期 14912
    assert_eq!(apu.pulse_outputs(), [15, 7]);

    advance(&mut apu, 1); // 总周期 14913：第二次 quarter
    assert_eq!(apu.pulse_outputs(), [14, 7]);

    advance(&mut apu, 7_457); // 总周期 22370
    assert_eq!(apu.pulse_outputs(), [14, 7]);

    advance(&mut apu, 1); // 总周期 22371：第三次 quarter
    assert_eq!(apu.pulse_outputs(), [13, 7]);
}

#[test]
fn lengths_advance_automatically_and_respect_halt() {
    let mut apu = Apu::new();
    apu.write_register(0x4015, 3);

    // 两个声道初始长度均为 2。
    // Pulse 1 正常倒计数；Pulse 2 设置 halt。
    configure_pulse(&mut apu, 0x4000, 0x17, 0x18);
    configure_pulse(&mut apu, 0x4004, 0x37, 0x18);

    advance(&mut apu, 14_912);
    assert_eq!(apu.read_status() & 3, 3);

    advance(&mut apu, 1); // 第一次 half：Pulse 1 长度 2→1
    assert_eq!(apu.read_status() & 3, 3);

    advance(&mut apu, 14_915); // 总周期 29828
    assert_eq!(apu.read_status() & 3, 3);

    advance(&mut apu, 1); // 第二次 half：Pulse 1 长度 1→0
    assert_eq!(apu.read_status() & 3, 2);
    assert_eq!(apu.pulse_outputs()[0], 0);

    // 解除 Pulse 2 的 halt；其长度仍然是 2。
    apu.write_register(0x4004, 0x17);

    advance(&mut apu, 14_914); // 总周期 44743：长度 2→1
    assert_eq!(apu.read_status() & 3, 2);

    advance(&mut apu, 14_916); // 总周期 59659：长度 1→0
    assert_eq!(apu.read_status() & 3, 0);
}

#[test]
fn frame_counter_runs_while_channels_are_disabled() {
    let mut apu = Apu::new();

    // 没有声道启用时，共享节奏仍然前进。
    advance(&mut apu, 10_000);

    apu.write_register(0x4015, 1);
    configure_pulse(&mut apu, 0x4000, 0x17, 0x18);

    // 启用和写声道寄存器，都不能重新开始共享节奏。
    advance(&mut apu, 19_828); // 总周期 29828
    assert_eq!(apu.read_status() & 1, 1);

    advance(&mut apu, 1); // 总周期 29829：第二次 half
    assert_eq!(apu.read_status() & 1, 0);
}
