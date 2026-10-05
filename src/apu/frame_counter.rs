#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct FrameEvents {
    pub(super) quarter: bool,
    pub(super) half: bool,
}

pub(super) struct FrameCounter {
    cycle: u32,
}

impl FrameCounter {
    pub(super) fn new() -> Self {
        Self { cycle: 0 }
    }

    // 每次调用代表经过一个 CPU 周期。
    pub(super) fn clock_cpu_cycle(&mut self) -> FrameEvents {
        self.cycle += 1;

        let (half, quarter) = match self.cycle {
            7457 => (false, true),
            14913 => (true, true),
            22371 => (false, true),
            29829 => (true, true),
            _ => (false, false),
        };
        if self.cycle == 29830 {
            self.cycle = 0;
        }
        FrameEvents { half, quarter }
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
}
