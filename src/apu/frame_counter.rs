#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct FrameEvents {
    pub(super) quarter: bool,
    pub(super) half: bool,
}

pub(super) struct FrameCounter {
    cycle: u32,
    cpu_cycles: u64,
    five_step: bool,
    pending_reset: Option<(bool, u8)>,
    block_ticks: u8,
}

impl FrameCounter {
    pub(super) fn new() -> Self {
        Self {
            cycle: 0,
            cpu_cycles: 0,
            five_step: false,
            pending_reset: None,
            block_ticks: 0,
        }
    }

    pub(super) fn write_register(&mut self, value: u8) {
        // 1. 从 bit 7 取得待采用的模式。
        // 2. 根据 cpu_cycles 的奇偶选择等待 3 或 4 周期。
        // 3. 保存到 pending_reset。
        let five_step = value & 0x80 != 0;
        let delay = if self.cpu_cycles & 1 == 0 { 3 } else { 4 };

        // 四步、五步都需要重置；新请求覆盖尚未完成的旧请求。
        self.pending_reset = Some((five_step, delay));
    }

    // 每次调用代表经过一个 CPU 周期。
    // pub(super) fn clock_cpu_cycle(&mut self) -> FrameEvents {
    //     self.cycle += 1;
    //     self.cpu_cycles = self.cpu_cycles.wrapping_add(1);

    //     let (half, quarter) = match self.cycle {
    //         7457 => (false, true),
    //         14913 => (true, true),
    //         22371 => (false, true),
    //         29829 => (true, true),
    //         _ => (false, false),
    //     };
    //     if self.cycle == 29830 {
    //         self.cycle = 0;
    //     }

    //     self.block_ticks = self.block_ticks.saturating_sub(1);
    //     FrameEvents { half, quarter }
    // }
    pub(super) fn clock_cpu_cycle(&mut self) -> FrameEvents {
        self.cpu_cycles = self.cpu_cycles.wrapping_add(1);
        self.cycle += 1;

        // 一、按当前已经生效的模式，判断正常更新事件。
        let scheduled = match (self.five_step, self.cycle) {
            (_, 7_457 | 22_371) => FrameEvents {
                quarter: true,
                half: false,
            },
            (_, 14_913) | (false, 29_829) | (true, 37_281) => FrameEvents {
                quarter: true,
                half: true,
            },
            _ => FrameEvents::default(),
        };

        // 自然归零不改变总 CPU 周期，也不重置声道状态。
        let sequence_length = if self.five_step { 37_282 } else { 29_830 };
        if self.cycle == sequence_length {
            self.cycle = 0;
        }

        let mut events = FrameEvents::default();

        // 二、正常事件未被抑制时，记录本次输出并开启保护。
        if self.block_ticks == 0 && (scheduled.quarter || scheduled.half) {
            events = scheduled;
            self.block_ticks = 2;
        }

        // 三、推进待处理请求。等待期间，上面的正常序列仍然运行。
        if let Some((five_step, remaining)) = self.pending_reset {
            if remaining > 1 {
                self.pending_reset = Some((five_step, remaining - 1));
            } else {
                // 请求到期：应用模式，重新开始序列。
                self.pending_reset = None;
                self.five_step = five_step;
                self.cycle = 0;

                // 五步模式额外触发一次 quarter + half。
                // 但是如果短期内（上一个cpu周期）触发过一次事件，这次额外事件不会触发
                // gpt没有找到具体的原因
                if self.five_step && self.block_ticks == 0 {
                    events = FrameEvents {
                        quarter: true,
                        half: true,
                    };
                    self.block_ticks = 2;
                }
            }
        }

        // 四、保护计数在所有事件处理完成后再减少。
        self.block_ticks = self.block_ticks.saturating_sub(1);

        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_step_events_repeat_at_exact_cpu_cycles() {
        let mut counter = FrameCounter::new();
        let mut observed = Vec::new();

        // 检查连续两轮；同时检查非事件周期没有错误地重复发信号。
        for total_cycle in 1u32..=59_660 {
            let events = counter.clock_cpu_cycle();

            let local_cycle = total_cycle % 29_830;
            let expected = FrameEvents {
                quarter: matches!(local_cycle, 7_457 | 14_913 | 22_371 | 29_829),
                half: matches!(local_cycle, 14_913 | 29_829),
            };

            assert_eq!(events, expected, "total CPU cycle={total_cycle}");

            if events.quarter || events.half {
                observed.push((total_cycle, events.quarter, events.half));
            }
        }

        assert_eq!(
            observed,
            vec![
                (7_457, true, false),
                (14_913, true, true),
                (22_371, true, false),
                (29_829, true, true),
                (37_287, true, false),
                (44_743, true, true),
                (52_201, true, false),
                (59_659, true, true),
            ]
        );
    }

    // 将整个模块追加到 /home/hgg/src/nes/src/apu/frame_counter.rs。
    // 保留原来的 tests 模块；这里使用不同的模块名。
    // 接口：FrameCounter::write_register(value: u8)。
    // 字段：cycle, cpu_cycles, five_step。
    #[cfg(test)]
    mod register_tests {
        use super::*;
        use crate::apu::Apu;

        fn none() -> FrameEvents {
            FrameEvents {
                quarter: false,
                half: false,
            }
        }

        fn quarter() -> FrameEvents {
            FrameEvents {
                quarter: true,
                half: false,
            }
        }

        fn both() -> FrameEvents {
            FrameEvents {
                quarter: true,
                half: true,
            }
        }

        fn run(counter: &mut FrameCounter, cycles: usize) {
            for _ in 0..cycles {
                counter.clock_cpu_cycle();
            }
        }

        #[test]
        fn five_step_reset_waits_three_or_four_following_cpu_cycles() {
            for (elapsed, delay) in [(0usize, 3usize), (1, 4)] {
                let mut counter = FrameCounter::new();
                run(&mut counter, elapsed);
                counter.write_register(0x80);
                assert!(!counter.five_step, "写入时不能提前应用待处理模式");

                for _ in 1..delay {
                    assert_eq!(counter.clock_cpu_cycle(), none());
                    assert!(!counter.five_step);
                }

                assert_eq!(counter.clock_cpu_cycle(), both());
                assert!(counter.five_step);
                assert_eq!(counter.cycle, 0);
                assert_eq!(counter.cpu_cycles, (elapsed + delay) as u64);
                assert_eq!(counter.clock_cpu_cycle(), none());
            }
        }

        #[test]
        fn five_step_schedule_repeats_without_an_event_at_29829() {
            let mut counter = FrameCounter::new();
            counter.write_register(0x80);
            assert_eq!(counter.clock_cpu_cycle(), none());
            assert_eq!(counter.clock_cpu_cycle(), none());
            assert_eq!(counter.clock_cpu_cycle(), both());

            for elapsed in 1u32..=74_564 {
                let local = elapsed % 37_282;
                let expected = FrameEvents {
                    quarter: matches!(local, 7_457 | 14_913 | 22_371 | 37_281),
                    half: matches!(local, 14_913 | 37_281),
                };
                assert_eq!(
                    counter.clock_cpu_cycle(),
                    expected,
                    "重置后的 CPU 周期={elapsed}"
                );
            }
            assert_eq!(counter.cycle, 0);
        }

        #[test]
        fn switching_back_to_four_step_resets_without_forced_clocks() {
            let mut counter = FrameCounter::new();
            counter.write_register(0x80);
            run(&mut counter, 3);

            // 总周期为 3，选择四步模式，延迟为 4。
            counter.write_register(0x40);
            for _ in 0..4 {
                assert_eq!(counter.clock_cpu_cycle(), none());
            }
            assert!(!counter.five_step);
            assert_eq!(counter.cycle, 0);

            for elapsed in 1u32..=29_830 {
                let expected = FrameEvents {
                    quarter: matches!(elapsed, 7_457 | 14_913 | 22_371 | 29_829),
                    half: matches!(elapsed, 14_913 | 29_829),
                };
                assert_eq!(counter.clock_cpu_cycle(), expected, "cycle={elapsed}");
            }
            assert_eq!(counter.cycle, 0);
        }

        #[test]
        fn an_unchanged_mode_write_still_restarts_the_sequence() {
            let mut counter = FrameCounter::new();
            run(&mut counter, 100);
            counter.write_register(0x40); // 仍是四步模式，也必须重置。
            for _ in 0..3 {
                assert_eq!(counter.clock_cpu_cycle(), none());
            }
            assert_eq!(counter.cycle, 0);
            assert_eq!(counter.cpu_cycles, 103);
            for _ in 0..7_456 {
                assert_eq!(counter.clock_cpu_cycle(), none());
            }
            assert_eq!(counter.clock_cpu_cycle(), quarter());
        }

        #[test]
        fn the_old_sequence_continues_during_the_reset_delay() {
            let mut counter = FrameCounter::new();
            run(&mut counter, 7_455);
            counter.write_register(0x80); // 奇数相位，等待 4 个周期。
            assert_eq!(counter.clock_cpu_cycle(), none()); // 7456
            assert_eq!(counter.clock_cpu_cycle(), quarter()); // 7457：原序列事件
            assert!(!counter.five_step);
            assert_eq!(counter.clock_cpu_cycle(), none()); // 7458
            assert_eq!(counter.clock_cpu_cycle(), both()); // 7459：重置事件
            assert_eq!(counter.cycle, 0);
            assert!(counter.five_step);
        }

        #[test]
        fn newest_write_replaces_the_pending_reset() {
            let mut counter = FrameCounter::new();
            counter.write_register(0x80);
            assert_eq!(counter.clock_cpu_cycle(), none());
            counter.write_register(0x40); // 周期 1，重新等待 4 周期。
            for _ in 0..4 {
                assert_eq!(counter.clock_cpu_cycle(), none());
            }
            assert!(!counter.five_step);
            assert_eq!(counter.cycle, 0);
            assert_eq!(counter.cpu_cycles, 5);
        }

        #[test]
        fn natural_sequence_wrap_does_not_reset_cpu_phase() {
            let mut counter = FrameCounter::new();
            counter.write_register(0x80);
            run(&mut counter, 3);
            run(&mut counter, 37_282);
            assert_eq!(counter.cycle, 0);
            assert_eq!(counter.cpu_cycles, 37_285);

            // 序列位置为偶数 0，但总周期为奇数，延迟必须是 4。
            counter.write_register(0x80);
            for _ in 0..3 {
                assert_eq!(counter.clock_cpu_cycle(), none());
            }
            assert_eq!(counter.clock_cpu_cycle(), both());
            assert_eq!(counter.cycle, 0);
        }

        #[test]
        fn regular_event_blocks_a_second_event_on_the_same_cycle() {
            let mut counter = FrameCounter::new();
            run(&mut counter, 7_454);
            counter.write_register(0x80); // 重置恰好落在原 quarter 的 7457。
            assert_eq!(counter.clock_cpu_cycle(), none());
            assert_eq!(counter.clock_cpu_cycle(), none());
            assert_eq!(counter.clock_cpu_cycle(), quarter());
            assert!(counter.five_step);
            assert_eq!(counter.cycle, 0); // 重置照常，只抑制额外更新。
        }

        #[test]
        fn regular_event_also_blocks_a_forced_event_on_the_next_cycle() {
            let mut counter = FrameCounter::new();
            counter.write_register(0x80);
            run(&mut counter, 3); // 第一次重置。
            run(&mut counter, 7_454); // 总周期 7457，序列位置 7454。

            counter.write_register(0x80); // 总周期是奇数，等待 4 周期。
            assert_eq!(counter.clock_cpu_cycle(), none());
            assert_eq!(counter.clock_cpu_cycle(), none());
            assert_eq!(counter.clock_cpu_cycle(), quarter()); // 总周期 7460
            assert_eq!(counter.clock_cpu_cycle(), none()); // 7461：抑制额外事件
            assert_eq!(counter.cycle, 0);
            assert!(counter.five_step);
        }

        #[test]
        fn apu_4017_write_clocks_both_channels_only_after_the_delay() {
            let mut apu = Apu::new();
            apu.write_register(0x4015, 3);
            for (base, control) in [(0x4000u16, 0x00u8), (0x4004, 0x17)] {
                apu.write_register(base, control);
                apu.write_register(base + 2, 8);
                apu.write_register(base + 3, 0x18); // 初始长度 2。
            }

            apu.write_register(0x4017, 0xC0);
            apu.clock_cpu_cycle();
            apu.clock_cpu_cycle();
            assert_eq!(apu.pulse_outputs(), [0, 7]);

            apu.clock_cpu_cycle(); // 第三周期：包络初始化，长度 2→1。
            assert_eq!(apu.pulse_outputs(), [15, 7]);
            assert_eq!(apu.read_status() & 3, 3);

            for _ in 0..14_912 {
                apu.clock_cpu_cycle();
            }
            assert_eq!(apu.read_status() & 3, 3);
            apu.clock_cpu_cycle(); // 重置后 14913：长度 1→0。
            assert_eq!(apu.read_status() & 3, 0);
            assert_eq!(apu.pulse_outputs(), [0, 0]);
        }
    }
}
