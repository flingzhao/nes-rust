//! SDL 前端的纯队列策略：不访问音频设备，也不推进模拟器时钟。

const START_BUFFER_MS: u64 = 40;
const HIGH_WATER_MS: u64 = 60;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PlaybackAction {
    /// 设备尚未启动，继续生成声音以填充预缓冲。
    Fill,
    /// 预缓冲已足够；调用方应启动设备，此动作只返回一次。
    Start,
    /// 已开始播放，继续模拟并向设备提交声音。
    Run,
    /// 队列已足够长；等待设备消耗，暂不推进模拟器。
    Wait,
}

pub(crate) struct PlaybackPolicy {
    started: bool,
    start_bytes: u32,
    high_water_bytes: u32,
}

impl PlaybackPolicy {
    pub(crate) fn new(sample_rate: u32) -> Result<Self, String> {
        if sample_rate == 0 {
            return Err("Audio sample rate must be greater than zero".to_string());
        }
        let bytes_for_ms = |milliseconds: u64| -> Result<u32, String> {
            // 向上取整到完整样本，再换算为单声道 f32 字节数。
            let samples = (u64::from(sample_rate) * milliseconds + 999) / 1000;
            u32::try_from(samples * std::mem::size_of::<f32>() as u64)
                .map_err(|_| "Audio queue threshold exceeds u32".to_string())
        };
        Ok(Self {
            started: false,
            start_bytes: bytes_for_ms(START_BUFFER_MS)?,
            high_water_bytes: bytes_for_ms(HIGH_WATER_MS)?,
        })
    }

    pub(crate) fn update(&mut self, queued_bytes: u32) -> PlaybackAction {
        if !self.started {
            // 即使预缓冲超过高水位，也必须先启动，否则暂停的设备无法消耗它。
            if queued_bytes >= self.start_bytes {
                self.started = true;
                return PlaybackAction::Start;
            }
            return PlaybackAction::Fill;
        }

        if queued_bytes >= self.high_water_bytes {
            PlaybackAction::Wait
        } else {
            // 本轮不重新预缓冲；队列空时也继续生成声音。
            PlaybackAction::Run
        }
    }
}

/// 仅调整送往设备的音量，不修改 APU、混音或滤波状态。
pub(crate) fn prepare_output(samples: &[f32], gain: f32) -> Result<Vec<f32>, String> {
    if !gain.is_finite() || !(0.0..=1.0).contains(&gain) {
        return Err("Playback gain must be finite and between 0 and 1".to_string());
    }
    if let Some(index) = samples.iter().position(|sample| !sample.is_finite()) {
        return Err(format!("Non-finite audio sample at index {index}"));
    }
    Ok(samples
        .iter()
        .map(|sample| (sample * gain).clamp(-1.0, 1.0))
        .collect())
}

#[cfg(test)]
mod audio_playback_tests {
    use super::{PlaybackAction, PlaybackPolicy, prepare_output};

    #[test]
    fn waits_for_forty_ms_before_starting() {
        let mut policy = PlaybackPolicy::new(48_000).unwrap();
        assert_eq!(policy.update(0), PlaybackAction::Fill);
        assert_eq!(policy.update(3_200), PlaybackAction::Fill);
        assert_eq!(policy.update(6_400), PlaybackAction::Fill);
        assert_eq!(policy.update(7_676), PlaybackAction::Fill);
        assert_eq!(policy.update(7_680), PlaybackAction::Start);
    }

    #[test]
    fn resumes_once_then_runs_without_rebuffering() {
        let mut policy = PlaybackPolicy::new(48_000).unwrap();
        assert_eq!(policy.update(9_600), PlaybackAction::Start);
        assert_eq!(policy.update(9_600), PlaybackAction::Run);
        assert_eq!(policy.update(7_680), PlaybackAction::Run);
        assert_eq!(policy.update(0), PlaybackAction::Run);
    }

    #[test]
    fn waits_at_sixty_ms_and_runs_again_below_it() {
        let mut policy = PlaybackPolicy::new(48_000).unwrap();
        assert_eq!(policy.update(7_680), PlaybackAction::Start);
        assert_eq!(policy.update(11_516), PlaybackAction::Run);
        assert_eq!(policy.update(11_520), PlaybackAction::Wait);
        assert_eq!(policy.update(14_720), PlaybackAction::Wait);
        assert_eq!(policy.update(11_516), PlaybackAction::Run);
    }

    #[test]
    fn large_initial_queue_starts_before_applying_backpressure() {
        let mut policy = PlaybackPolicy::new(48_000).unwrap();
        assert_eq!(policy.update(16_000), PlaybackAction::Start);
        assert_eq!(policy.update(16_000), PlaybackAction::Wait);
    }

    #[test]
    fn uses_requested_rate_instead_of_hardcoded_byte_counts() {
        let mut policy = PlaybackPolicy::new(44_100).unwrap();
        assert_eq!(policy.update(7_052), PlaybackAction::Fill);
        assert_eq!(policy.update(7_056), PlaybackAction::Start);
        assert_eq!(policy.update(10_580), PlaybackAction::Run);
        assert_eq!(policy.update(10_584), PlaybackAction::Wait);
    }

    #[test]
    fn rounds_partial_samples_up_without_integer_overflow() {
        let mut policy = PlaybackPolicy::new(44_101).unwrap();
        assert_eq!(policy.update(7_056), PlaybackAction::Fill);
        assert_eq!(policy.update(7_060), PlaybackAction::Start);
        assert_eq!(policy.update(10_584), PlaybackAction::Run);
        assert_eq!(policy.update(10_588), PlaybackAction::Wait);
        assert!(PlaybackPolicy::new(0).is_err());
        assert!(PlaybackPolicy::new(u32::MAX).is_ok());
    }

    #[test]
    fn scales_output_without_modifying_input() {
        let samples = [-1.0, -0.5, 0.0, 0.5, 1.0];
        let output = prepare_output(&samples, 0.25).unwrap();
        assert_eq!(output, [-0.25, -0.125, 0.0, 0.125, 0.25]);
        assert_eq!(samples, [-1.0, -0.5, 0.0, 0.5, 1.0]);
    }

    #[test]
    fn applies_gain_before_clamping_and_preserves_negative_samples() {
        let output = prepare_output(&[-3.0, -1.5, 0.0, 1.5, 3.0], 0.5).unwrap();
        assert_eq!(output, [-1.0, -0.75, 0.0, 0.75, 1.0]);
    }

    #[test]
    fn handles_empty_buffers_and_zero_gain() {
        assert!(prepare_output(&[], 1.0).unwrap().is_empty());
        assert_eq!(prepare_output(&[-0.5, 0.5], 0.0).unwrap(), [0.0, 0.0]);
    }

    #[test]
    fn rejects_nonfinite_samples_and_invalid_gain() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(prepare_output(&[0.0, bad], 0.25).is_err());
            assert!(prepare_output(&[bad], 0.0).is_err());
        }
        for bad in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
            assert!(prepare_output(&[0.0], bad).is_err());
            assert!(prepare_output(&[], bad).is_err());
        }
    }
}
