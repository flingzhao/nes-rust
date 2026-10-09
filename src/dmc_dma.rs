#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CpuBusCycle {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DmaPhase {
    Get,
    Put,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DmcDmaAction {
    Pass,
    Halt,
    Dummy,
    Align,
    Read(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DmcDmaState {
    Idle,
    WaitingForHalt(u16),
    Dummy(u16),
    WaitingForRead(u16),
}

pub(crate) struct DmcDma {
    state: DmcDmaState,
}

impl DmcDma {
    pub(crate) fn new() -> Self {
        Self {
            state: DmcDmaState::Idle,
        }
    }

    pub(crate) fn request(&mut self, addr: u16) -> Result<(), String> {
        match self.state {
            DmcDmaState::Idle => self.state = DmcDmaState::WaitingForHalt(addr),
            _ => {
                return Err("current state is not idle".to_string());
            }
        };
        Ok(())
    }

    pub(crate) fn clock(&mut self, phase: DmaPhase, cpu_cycle: CpuBusCycle) -> DmcDmaAction {
        match self.state {
            DmcDmaState::Idle => DmcDmaAction::Pass,
            DmcDmaState::WaitingForHalt(addr) => match cpu_cycle {
                CpuBusCycle::Write => DmcDmaAction::Pass,
                CpuBusCycle::Read => {
                    self.state = DmcDmaState::Dummy(addr);
                    DmcDmaAction::Halt
                }
            },
            DmcDmaState::Dummy(addr) => {
                self.state = DmcDmaState::WaitingForRead(addr);
                DmcDmaAction::Dummy
            }
            DmcDmaState::WaitingForRead(addr) => match phase {
                DmaPhase::Put => DmcDmaAction::Align,
                DmaPhase::Get => {
                    self.state = DmcDmaState::Idle;
                    DmcDmaAction::Read(addr)
                }
            },
        }
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.state != DmcDmaState::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::{CpuBusCycle, DmaPhase, DmcDma, DmcDmaAction};

    #[test]
    fn idle_allows_cpu_to_run() {
        let mut dma = DmcDma::new();
        assert!(!dma.is_busy());
        for phase in [DmaPhase::Get, DmaPhase::Put] {
            for cpu in [CpuBusCycle::Read, CpuBusCycle::Write] {
                assert_eq!(dma.clock(phase, cpu), DmcDmaAction::Pass);
            }
        }
    }

    #[test]
    fn halt_on_get_takes_three_stalled_cycles() {
        let mut dma = DmcDma::new();
        dma.request(0xC080).unwrap();
        assert!(dma.is_busy());
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read),
            DmcDmaAction::Halt
        );
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read),
            DmcDmaAction::Dummy
        );
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read),
            DmcDmaAction::Read(0xC080)
        );
        assert!(!dma.is_busy());
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read),
            DmcDmaAction::Pass
        );
    }

    #[test]
    fn halt_on_put_takes_four_stalled_cycles() {
        let mut dma = DmcDma::new();
        dma.request(0xC080).unwrap();
        let phases = [DmaPhase::Put, DmaPhase::Get, DmaPhase::Put, DmaPhase::Get];
        let expected = [
            DmcDmaAction::Halt,
            DmcDmaAction::Dummy,
            DmcDmaAction::Align,
            DmcDmaAction::Read(0xC080),
        ];
        for (phase, action) in phases.into_iter().zip(expected) {
            assert_eq!(dma.clock(phase, CpuBusCycle::Read), action);
        }
        assert!(!dma.is_busy());
    }

    #[test]
    fn cpu_write_delays_halt_without_consuming_request() {
        let mut dma = DmcDma::new();
        dma.request(0xC080).unwrap();
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Write),
            DmcDmaAction::Pass
        );
        assert!(dma.is_busy());
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read),
            DmcDmaAction::Halt
        );
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read),
            DmcDmaAction::Dummy
        );
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read),
            DmcDmaAction::Align
        );
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read),
            DmcDmaAction::Read(0xC080)
        );
    }

    #[test]
    fn consecutive_cpu_writes_are_not_halted() {
        let mut dma = DmcDma::new();
        dma.request(0x8000).unwrap();
        for phase in [DmaPhase::Put, DmaPhase::Get, DmaPhase::Put] {
            assert_eq!(dma.clock(phase, CpuBusCycle::Write), DmcDmaAction::Pass);
        }
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read),
            DmcDmaAction::Halt
        );
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read),
            DmcDmaAction::Dummy
        );
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read),
            DmcDmaAction::Read(0x8000)
        );
    }

    #[test]
    fn busy_requests_cannot_overwrite_address() {
        let mut dma = DmcDma::new();
        dma.request(0xC080).unwrap();
        assert!(dma.request(0xC081).is_err());
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read),
            DmcDmaAction::Halt
        );
        assert!(dma.request(0xC082).is_err());
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read),
            DmcDmaAction::Dummy
        );
        assert!(dma.request(0xC083).is_err());
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read),
            DmcDmaAction::Align
        );
        assert!(dma.request(0xC084).is_err());
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read),
            DmcDmaAction::Read(0xC080)
        );
    }

    #[test]
    fn next_transfer_uses_external_phase_not_a_reset_local_phase() {
        let mut dma = DmcDma::new();
        dma.request(0xC080).unwrap();
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read),
            DmcDmaAction::Halt
        );
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read),
            DmcDmaAction::Dummy
        );
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read),
            DmcDmaAction::Read(0xC080)
        );
        dma.request(0xC081).unwrap();
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read),
            DmcDmaAction::Halt
        );
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read),
            DmcDmaAction::Dummy
        );
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read),
            DmcDmaAction::Align
        );
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read),
            DmcDmaAction::Read(0xC081)
        );
    }

    #[test]
    fn sample_read_occurs_once_and_only_at_the_read_action() {
        let mut dma = DmcDma::new();
        dma.request(0xC080).unwrap();
        let phases = [
            DmaPhase::Put,
            DmaPhase::Get,
            DmaPhase::Put,
            DmaPhase::Get,
            DmaPhase::Put,
        ];
        let mut sample_buffer = None;
        let mut sample_reads = Vec::new();
        let mut stalled_cycles = 0;
        let mut cpu_completed_cycles = 0;

        for (index, phase) in phases.into_iter().enumerate() {
            match dma.clock(phase, CpuBusCycle::Read) {
                DmcDmaAction::Pass => cpu_completed_cycles += 1,
                DmcDmaAction::Read(addr) => {
                    stalled_cycles += 1;
                    sample_reads.push(addr);
                    assert_eq!(addr, 0xC080);
                    // Test-only bus result: memory[$C080] = $A5.
                    sample_buffer = Some(0xA5u8);
                }
                DmcDmaAction::Halt | DmcDmaAction::Dummy | DmcDmaAction::Align => {
                    stalled_cycles += 1;
                }
            }
            if index < 3 {
                assert_eq!(sample_buffer, None);
            }
        }
        assert_eq!(sample_reads, vec![0xC080]);
        assert_eq!(sample_buffer, Some(0xA5));
        assert_eq!(stalled_cycles, 4);
        assert_eq!(cpu_completed_cycles, 1);
    }
}
