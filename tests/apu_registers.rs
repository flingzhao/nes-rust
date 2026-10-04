use nes::apu::Apu;

fn clock(apu: &mut Apu, cycles: usize) {
    for _ in 0..cycles {
        apu.clock_cpu_cycle();
    }
}

fn configured_pulse1() -> Apu {
    let mut apu = Apu::new();
    apu.write_register(0x4015, 0x01);
    apu.write_register(0x4000, 0x1A); // duty 0，固定音量 10，不暂停长度
    apu.write_register(0x4002, 8);
    apu.write_register(0x4003, 0x18); // 长度编号 3 -> 长度 2
    apu
}

#[test]
fn enabling_does_not_start_or_restore_a_note() {
    let mut apu = Apu::new();

    // 禁用期间写高位，不装载长度。
    apu.write_register(0x4000, 0x1A);
    apu.write_register(0x4002, 8);
    apu.write_register(0x4003, 0x18);
    assert_eq!(apu.read_status(), 0);

    apu.write_register(0x4015, 1);
    assert_eq!(apu.read_status(), 0);

    apu.write_register(0x4003, 0x18);
    assert_eq!(apu.read_status(), 1);

    apu.write_register(0x4015, 0);
    assert_eq!(apu.read_status(), 0);
    assert_eq!(apu.pulse_outputs(), [0, 0]);

    apu.write_register(0x4015, 1);
    assert_eq!(apu.read_status(), 0);

    apu.write_register(0x4003, 0x18);
    assert_eq!(apu.read_status(), 1);
}

#[test]
fn pulses_have_independent_registers_and_enable_bits() {
    let mut apu = configured_pulse1();

    apu.write_register(0x4015, 3);
    apu.write_register(0x4004, 0x97); // duty 2，固定音量 7
    apu.write_register(0x4006, 8);
    apu.write_register(0x4007, 0x18);

    // 初始 timer 为 0，寄存器写入不重装当前 timer。
    // 第 2 个 CPU 周期，两路都首次进入序列位置 1。
    clock(&mut apu, 2);
    assert_eq!(apu.pulse_outputs(), [10, 7]);
    assert_eq!(apu.read_status(), 3);

    // 再过 18 周期，两路进入位置 2。
    clock(&mut apu, 18);
    assert_eq!(apu.pulse_outputs(), [0, 7]);

    apu.write_register(0x4015, 2);
    assert_eq!(apu.read_status(), 2);
    assert_eq!(apu.pulse_outputs(), [0, 7]);
}

#[test]
fn status_reports_length_not_current_output() {
    let mut apu = configured_pulse1();

    // 音量为零，但长度仍有效。
    apu.write_register(0x4000, 0x10);
    clock(&mut apu, 2);
    assert_eq!(apu.pulse_outputs(), [0, 0]);
    assert_eq!(apu.read_status(), 1);

    // 周期小于 8 导致静音，也不清除长度。
    apu.write_register(0x4000, 0x1A);
    apu.write_register(0x4002, 7);
    assert_eq!(apu.pulse_outputs(), [0, 0]);
    assert_eq!(apu.read_status(), 1);
}

#[test]
fn length_expires_and_halt_prevents_decrement() {
    let mut apu = configured_pulse1();

    apu.clock_half_frame();
    assert_eq!(apu.read_status(), 1);

    apu.clock_half_frame();
    assert_eq!(apu.read_status(), 0);
    assert_eq!(apu.pulse_outputs(), [0, 0]);

    // 零不能下溢，也不能因为设置 halt 而恢复。
    apu.write_register(0x4000, 0x3A);
    apu.clock_half_frame();
    assert_eq!(apu.read_status(), 0);

    apu.write_register(0x4003, 0x18);
    for _ in 0..10 {
        apu.clock_half_frame();
    }
    assert_eq!(apu.read_status(), 1);

    // 取消 halt 后，仍需要两次更新才到零。
    apu.write_register(0x4000, 0x1A);
    apu.clock_half_frame();
    assert_eq!(apu.read_status(), 1);
    apu.clock_half_frame();
    assert_eq!(apu.read_status(), 0);
}

#[test]
fn loads_all_length_table_entries() {
    let lengths: [u8; 32] = [
        10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96,
        22, 192, 24, 72, 26, 16, 28, 32, 30,
    ];

    for (index, length) in lengths.into_iter().enumerate() {
        let mut apu = configured_pulse1();
        apu.write_register(0x4003, (index as u8) << 3);

        for remaining in (1..=length).rev() {
            assert_eq!(
                apu.read_status() & 1,
                1,
                "index={index}, remaining={remaining}"
            );
            apu.clock_half_frame();
        }

        assert_eq!(apu.read_status() & 1, 0, "index={index}");
    }
}

#[test]
fn high_write_resets_sequence_but_not_timer_or_cpu_phase() {
    let mut apu = configured_pulse1();

    clock(&mut apu, 7);
    // 位置 1；timer=6；刚经过奇数个 CPU 周期。
    assert_eq!(apu.pulse_outputs()[0], 10);

    apu.write_register(0x4003, 0x18);
    assert_eq!(apu.pulse_outputs()[0], 0); // 位置重置为 0

    clock(&mut apu, 12);
    assert_eq!(apu.pulse_outputs()[0], 0);

    // 保留原 timer 和二分频状态，13 周期后前进到位置 1。
    clock(&mut apu, 1);
    assert_eq!(apu.pulse_outputs()[0], 10);
}

#[test]
fn low_write_preserves_sequence_and_current_countdown() {
    let mut apu = configured_pulse1();
    clock(&mut apu, 7);
    assert_eq!(apu.pulse_outputs()[0], 10);

    apu.write_register(0x4002, 9);
    assert_eq!(apu.pulse_outputs()[0], 10); // 不重置位置

    clock(&mut apu, 12);
    assert_eq!(apu.pulse_outputs()[0], 10);

    clock(&mut apu, 1);
    assert_eq!(apu.pulse_outputs()[0], 0); // 原倒计数到期，进入位置 2

    // 新周期每步为 20 CPU 周期。
    // 从位置 2 到下一次位置 1，需要 7 步。
    clock(&mut apu, 139);
    assert_eq!(apu.pulse_outputs()[0], 0);
    clock(&mut apu, 1);
    assert_eq!(apu.pulse_outputs()[0], 10);
}

#[test]
fn high_and_low_writes_preserve_the_other_period_bits() {
    let mut apu = configured_pulse1();

    apu.write_register(0x4003, 0x19); // period = $108
    apu.write_register(0x4002, 0xFD); // period = $1FD = 509

    clock(&mut apu, 2); // 初次装载 509，进入位置 1
    assert_eq!(apu.pulse_outputs()[0], 10);

    clock(&mut apu, 1019);
    assert_eq!(apu.pulse_outputs()[0], 10);
    clock(&mut apu, 1); // 2 * (509 + 1) = 1020
    assert_eq!(apu.pulse_outputs()[0], 0);

    apu.write_register(0x4003, 0x18); // 高位清零，低位应保留 $FD

    // 当前倒计数仍为 509；下一次才装载 253。
    clock(&mut apu, 1020);
    assert_eq!(apu.pulse_outputs()[0], 10);
    clock(&mut apu, 507);
    assert_eq!(apu.pulse_outputs()[0], 10);
    clock(&mut apu, 1);
    assert_eq!(apu.pulse_outputs()[0], 0);
}
