use nes::apu::Apu;

fn cpu_cycles(apu: &mut Apu, cycles: usize) {
    for _ in 0..cycles {
        apu.clock_cpu_cycle();
    }
}

fn configured(control1: u8, control2: u8) -> Apu {
    let mut apu = Apu::new();
    apu.write_register(0x4015, 3);

    for (base, control) in [(0x4000u16, control1), (0x4004, control2)] {
        apu.write_register(base, control);
        apu.write_register(base + 2, 8);
        apu.write_register(base + 3, 0x18); // 长度为 2，请求包络重新开始
    }

    // 两个发生器都进入波形位置 1，此时为高电平。
    cpu_cycles(&mut apu, 2);
    apu
}

#[test]
fn period_two_decays_once_per_three_updates() {
    let mut apu = configured(0x02, 0x17);

    // Pulse 1 的重新开始请求尚未处理。
    // Pulse 2 固定音量为 7。
    assert_eq!(apu.pulse_outputs(), [0, 7]);

    apu.clock_quarter_frame();
    assert_eq!(apu.pulse_outputs(), [15, 7]);

    for expected in [15, 15, 14, 14, 14, 13] {
        apu.clock_quarter_frame();
        assert_eq!(apu.pulse_outputs(), [expected, 7]);
    }

    // 包络更新没有减少长度。
    assert_eq!(apu.read_status() & 3, 3);
}

#[test]
fn every_period_uses_period_plus_one_updates_and_stops_at_zero() {
    for period in 0u8..=15 {
        let mut apu = configured(period, 0x10);
        apu.clock_quarter_frame(); // 重新开始：等级 15

        for target in (0u8..15).rev() {
            for _ in 0..period {
                apu.clock_quarter_frame();
                assert_eq!(
                    apu.pulse_outputs()[0],
                    target + 1,
                    "period={period}, target={target}"
                );
            }

            apu.clock_quarter_frame();
            assert_eq!(apu.pulse_outputs()[0], target);
        }

        for _ in 0..32 {
            apu.clock_quarter_frame();
            assert_eq!(apu.pulse_outputs()[0], 0);
        }
    }
}

#[test]
fn high_write_requests_restart_for_the_next_update() {
    let mut apu = configured(0x00, 0x10);

    for _ in 0..3 {
        apu.clock_quarter_frame();
    }
    assert_eq!(apu.pulse_outputs()[0], 13);

    apu.write_register(0x4003, 0x18);

    // 高位写入还会重置波形位置；先让它回到高电平。
    // CPU 周期推进本身不应触发本轮手动调度的包络更新。
    cpu_cycles(&mut apu, 18);
    assert_eq!(apu.pulse_outputs()[0], 13);

    apu.clock_quarter_frame();
    assert_eq!(apu.pulse_outputs()[0], 15);
}

#[test]
fn constant_volume_does_not_freeze_internal_decay() {
    let mut apu = configured(0x12, 0x10);

    // 固定输出 2；内部仍按参数 2 更新衰减状态。
    for _ in 0..4 {
        apu.clock_quarter_frame();
        assert_eq!(apu.pulse_outputs()[0], 2);
    }

    // 此时内部衰减等级应为 14。
    // 切换音量来源，不应重新开始包络。
    apu.write_register(0x4000, 0x02);
    assert_eq!(apu.pulse_outputs()[0], 14);

    apu.clock_quarter_frame();
    assert_eq!(apu.pulse_outputs()[0], 14);
}

#[test]
fn control_write_does_not_reload_the_current_divider() {
    let mut apu = configured(0x02, 0x10);

    apu.clock_quarter_frame(); // decay=15, divider=2
    apu.clock_quarter_frame(); // divider=1

    apu.write_register(0x4000, 0x00); // 新重装值为 0，当前 divider 仍为 1

    apu.clock_quarter_frame(); // divider=0
    assert_eq!(apu.pulse_outputs()[0], 15);

    apu.clock_quarter_frame(); // 重装 0，decay=14
    assert_eq!(apu.pulse_outputs()[0], 14);
}

#[test]
fn loop_restarts_after_zero_and_shares_the_length_halt_bit() {
    let mut apu = configured(0x00, 0x10);

    // 第一次初始化为 15，之后 15 次下降到 0。
    for _ in 0..16 {
        apu.clock_quarter_frame();
    }
    assert_eq!(apu.pulse_outputs()[0], 0);

    // 开启循环；写控制寄存器本身不重置衰减等级。
    apu.write_register(0x4000, 0x20);
    assert_eq!(apu.pulse_outputs()[0], 0);

    apu.clock_quarter_frame();
    assert_eq!(apu.pulse_outputs()[0], 15);

    for _ in 0..10 {
        apu.clock_half_frame();
    }
    assert_eq!(apu.read_status() & 1, 1); // 同一个 bit 5 暂停长度倒计数

    apu.write_register(0x4000, 0x00);
    apu.clock_half_frame();
    assert_eq!(apu.read_status() & 1, 1);
    apu.clock_half_frame();
    assert_eq!(apu.read_status() & 1, 0);
    assert_eq!(apu.pulse_outputs()[0], 0);
}

#[test]
fn pulse_envelopes_restart_independently() {
    // duty=2，位置 1 和 2 都是高电平。
    let mut apu = configured(0x80, 0x80);

    apu.clock_quarter_frame();
    apu.clock_quarter_frame();
    assert_eq!(apu.pulse_outputs(), [14, 14]);

    apu.write_register(0x4003, 0x18); // 只重新开始 Pulse 1
    cpu_cycles(&mut apu, 18);
    assert_eq!(apu.pulse_outputs(), [14, 14]);

    apu.clock_quarter_frame();
    assert_eq!(apu.pulse_outputs(), [15, 13]);
}

#[test]
fn restart_and_envelope_updates_also_work_while_disabled() {
    let mut apu = configured(0x00, 0x10);
    apu.write_register(0x4015, 2); // 禁用 Pulse 1
    apu.write_register(0x4003, 0x18);

    apu.clock_quarter_frame();
    assert_eq!(apu.pulse_outputs()[0], 0); // 内部更新，但禁用时仍不输出

    apu.write_register(0x4015, 3);
    apu.write_register(0x4003, 0x18); // 装载长度，并再次请求重新开始
    cpu_cycles(&mut apu, 18);

    // 尚未处理第二次请求；读到的是禁用期间已经更新到的等级 15。
    assert_eq!(apu.pulse_outputs()[0], 15);
}
