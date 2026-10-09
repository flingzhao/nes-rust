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
    WaitingForLoad { addr: u16, cycles_until_attempt: u8 },
    WaitingForReload(u16),
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

    #[cfg(test)]
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
            DmcDmaState::WaitingForHalt(addr) => self.try_halt(addr, cpu_cycle),
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
            DmcDmaState::WaitingForLoad {
                addr,
                cycles_until_attempt,
            } => {
                if cycles_until_attempt > 1 {
                    self.state = DmcDmaState::WaitingForLoad {
                        addr,
                        cycles_until_attempt: cycles_until_attempt - 1,
                    };
                    DmcDmaAction::Pass
                } else {
                    self.try_halt(addr, cpu_cycle)
                }
            }
            DmcDmaState::WaitingForReload(addr) => match phase {
                DmaPhase::Put => self.try_halt(addr, cpu_cycle),
                DmaPhase::Get => DmcDmaAction::Pass,
            },
        }
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.state != DmcDmaState::Idle
    }

    pub(crate) fn request_load(&mut self, addr: u16, write_phase: DmaPhase) -> Result<(), String> {
        if self.is_busy() {
            return Err("DMC DMA already has a pending transfer".to_string());
        }

        let cycles_until_attempt = if write_phase == DmaPhase::Get { 4 } else { 3 };

        self.state = DmcDmaState::WaitingForLoad {
            addr,
            cycles_until_attempt,
        };
        Ok(())
    }

    pub(crate) fn request_reload(&mut self, addr: u16) -> Result<(), String> {
        if self.is_busy() {
            return Err("DMC DMA already has a pending transfer".to_string());
        }

        self.state = DmcDmaState::WaitingForReload(addr);
        Ok(())
    }

    fn try_halt(&mut self, addr: u16, cpu_cycle: CpuBusCycle) -> DmcDmaAction {
        match cpu_cycle {
            CpuBusCycle::Write => {
                self.state = DmcDmaState::WaitingForHalt(addr);
                DmcDmaAction::Pass
            }
            CpuBusCycle::Read => {
                self.state = DmcDmaState::Dummy(addr);
                DmcDmaAction::Halt
            }
        }
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

// Append to src/dmc_dma.rs; keep the previous tests.
// Convention: request_load is called after the $4015 write-cycle DMA decision;
// the next clock call represents the CPU cycle AFTER that write.
// request_reload becomes visible on the next clock call as well.
#[cfg(test)]
mod start_timing_tests {
    use super::{CpuBusCycle, DmaPhase, DmcDma, DmcDmaAction};
    use CpuBusCycle::{Read as CpuRead, Write as CpuWrite};
    use DmaPhase::{Get, Put};
    use DmcDmaAction::{Align, Dummy, Halt, Pass, Read};

    fn check(dma: &mut DmcDma, trace: &[(DmaPhase, CpuBusCycle, DmcDmaAction)]) {
        for (index, &(phase, cpu, expected)) in trace.iter().enumerate() {
            assert_eq!(dma.clock(phase, cpu), expected, "clock {}", index + 1);
        }
    }

    #[test]
    fn load_after_put_write_attempts_halt_on_third_following_cycle() {
        let mut dma = DmcDma::new();
        dma.request_load(0xC080, Put).unwrap();
        assert!(dma.is_busy());
        check(
            &mut dma,
            &[
                (Get, CpuRead, Pass),
                (Put, CpuRead, Pass),
                (Get, CpuRead, Halt),
                (Put, CpuRead, Dummy),
                (Get, CpuRead, Read(0xC080)),
                (Put, CpuRead, Pass),
            ],
        );
        assert!(!dma.is_busy());
    }

    #[test]
    fn load_after_get_write_attempts_halt_on_fourth_following_cycle() {
        let mut dma = DmcDma::new();
        dma.request_load(0xC080, Get).unwrap();
        check(
            &mut dma,
            &[
                (Put, CpuRead, Pass),
                (Get, CpuRead, Pass),
                (Put, CpuRead, Pass),
                (Get, CpuRead, Halt),
                (Put, CpuRead, Dummy),
                (Get, CpuRead, Read(0xC080)),
            ],
        );
        assert!(!dma.is_busy());
    }

    #[test]
    fn writes_during_load_delay_do_not_freeze_the_countdown() {
        let mut dma = DmcDma::new();
        dma.request_load(0xC080, Put).unwrap();
        check(
            &mut dma,
            &[
                (Get, CpuWrite, Pass),
                (Put, CpuWrite, Pass),
                (Get, CpuRead, Halt),
                (Put, CpuRead, Dummy),
                (Get, CpuRead, Read(0xC080)),
            ],
        );
    }

    #[test]
    fn blocked_load_retries_on_the_next_cycle_even_if_it_is_put() {
        let mut dma = DmcDma::new();
        dma.request_load(0xC080, Put).unwrap();
        check(
            &mut dma,
            &[
                (Get, CpuRead, Pass),
                (Put, CpuRead, Pass),
                (Get, CpuWrite, Pass), // First halt attempt fails.
                (Put, CpuRead, Halt),  // Do not wait for another Get.
                (Get, CpuRead, Dummy),
                (Put, CpuRead, Align),
                (Get, CpuRead, Read(0xC080)),
            ],
        );
    }

    #[test]
    fn reload_waits_for_the_first_put_phase() {
        let mut dma = DmcDma::new();
        dma.request_reload(0xC081).unwrap();
        assert!(dma.is_busy());
        check(
            &mut dma,
            &[
                (Get, CpuRead, Pass),
                (Put, CpuRead, Halt),
                (Get, CpuRead, Dummy),
                (Put, CpuRead, Align),
                (Get, CpuRead, Read(0xC081)),
            ],
        );
    }

    #[test]
    fn reload_can_attempt_halt_on_the_next_call_if_it_is_put() {
        let mut dma = DmcDma::new();
        dma.request_reload(0xC081).unwrap();
        check(
            &mut dma,
            &[
                (Put, CpuRead, Halt),
                (Get, CpuRead, Dummy),
                (Put, CpuRead, Align),
                (Get, CpuRead, Read(0xC081)),
            ],
        );
    }

    #[test]
    fn blocked_reload_retries_on_get_and_then_takes_three_stalled_cycles() {
        let mut dma = DmcDma::new();
        dma.request_reload(0xC081).unwrap();
        check(
            &mut dma,
            &[
                (Put, CpuWrite, Pass),
                (Get, CpuRead, Halt),
                (Put, CpuRead, Dummy),
                (Get, CpuRead, Read(0xC081)),
            ],
        );
    }

    #[test]
    fn duplicate_requests_preserve_original_address_and_load_countdown() {
        let mut dma = DmcDma::new();
        dma.request_load(0xC080, Put).unwrap();
        assert_eq!(dma.clock(Get, CpuRead), Pass);
        assert!(dma.request_load(0xC090, Get).is_err());
        assert!(dma.request_reload(0xC091).is_err());
        assert!(dma.request(0xC092).is_err());
        check(
            &mut dma,
            &[
                (Put, CpuRead, Pass),
                (Get, CpuRead, Halt),
                (Put, CpuRead, Dummy),
                (Get, CpuRead, Read(0xC080)),
            ],
        );
    }

    #[test]
    fn new_request_apis_cannot_overwrite_waiting_or_running_reload() {
        let mut dma = DmcDma::new();
        dma.request_reload(0xC081).unwrap();
        for expected in [Pass, Halt, Dummy, Align] {
            assert!(dma.request_load(0xD000, Get).is_err());
            assert!(dma.request_reload(0xD001).is_err());
            let phase = match expected {
                Pass | Dummy => Get,
                Halt | Align => Put,
                _ => unreachable!(),
            };
            assert_eq!(dma.clock(phase, CpuRead), expected);
        }
        assert_eq!(dma.clock(Get, CpuRead), Read(0xC081));
        assert!(!dma.is_busy());
        assert!(dma.request_load(0xD000, Put).is_ok());
    }
}
