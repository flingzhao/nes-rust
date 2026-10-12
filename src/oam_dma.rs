use crate::dmc_dma::{CpuBusCycle, DmaPhase};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OamDmaAction {
    Pass,
    Halt,
    Align,
    Read(u16),
    Write(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OamDmaState {
    Idle,
    WaitingForHalt,
    WaitingForRead,
    AwaitingReadCompletion,
    WaitingForWrite(u8),
}

pub(crate) struct OamDma {
    state: OamDmaState,
    page: u8,
    offset: u16,
}

impl OamDma {
    pub(crate) fn new() -> Self {
        Self {
            state: OamDmaState::Idle,
            page: 0,
            offset: 0,
        }
    }

    pub(crate) fn request(&mut self, page: u8) -> Result<(), String> {
        match self.state {
            OamDmaState::Idle => {
                self.page = page;
                self.offset = 0;
                self.state = OamDmaState::WaitingForHalt;
                Ok(())
            }
            OamDmaState::WaitingForHalt => {
                self.page = page;
                self.offset = 0;
                Ok(())
            }
            _ => {
                return Err("other oam dma state, return error".to_string());
            }
        }
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.state != OamDmaState::Idle
    }

    pub(crate) fn clock(
        &mut self,
        phase: DmaPhase,
        cpu_cycle: CpuBusCycle,
    ) -> Result<OamDmaAction, String> {
        match self.state {
            OamDmaState::Idle => Ok(OamDmaAction::Pass),
            OamDmaState::WaitingForHalt => match cpu_cycle {
                CpuBusCycle::Read => {
                    self.state = OamDmaState::WaitingForRead;
                    Ok(OamDmaAction::Halt)
                }
                CpuBusCycle::Write => Ok(OamDmaAction::Pass),
            },
            OamDmaState::WaitingForRead => match phase {
                DmaPhase::Put => Ok(OamDmaAction::Align),
                DmaPhase::Get => {
                    self.state = OamDmaState::AwaitingReadCompletion;
                    Ok(OamDmaAction::Read(
                        (u16::from(self.page) << 8) | self.offset,
                    ))
                }
            },
            OamDmaState::WaitingForWrite(value) => match phase {
                DmaPhase::Get => Ok(OamDmaAction::Align),
                DmaPhase::Put => {
                    if self.offset < 255 {
                        self.offset += 1;
                        self.state = OamDmaState::WaitingForRead;
                    } else {
                        self.state = OamDmaState::Idle;
                    }
                    Ok(OamDmaAction::Write(value))
                }
            },
            OamDmaState::AwaitingReadCompletion => {
                return Err("invoke clock again while state == AwaitingReadCompletion".to_string());
            }
        }
    }

    pub(crate) fn complete_read(&mut self, value: u8) -> Result<(), String> {
        if self.state != OamDmaState::AwaitingReadCompletion {
            return Err(
                "complete_read can only be invoked in AwaitingReadCompletion state".to_string(),
            );
        }
        self.state = OamDmaState::WaitingForWrite(value);
        Ok(())
    }
}

#[cfg(test)]
mod oam_dma_controller_tests {
    use super::{OamDma, OamDmaAction};
    use crate::dmc_dma::{CpuBusCycle, DmaPhase};

    fn next_phase(phase: DmaPhase) -> DmaPhase {
        match phase {
            DmaPhase::Get => DmaPhase::Put,
            DmaPhase::Put => DmaPhase::Get,
        }
    }

    fn source_byte(offset: u8) -> u8 {
        offset.rotate_left(3) ^ 0xA5
    }

    // One iteration represents one elapsed CPU cycle. complete_read() belongs
    // to the SAME iteration as Read; it must not consume an additional cycle.
    fn transfer(first_phase: DmaPhase, stolen_cycle: Option<usize>) -> usize {
        let mut dma = OamDma::new();
        dma.request(0x02).unwrap();
        let mut phase = first_phase;
        let mut cycles = 0;
        let mut reads = Vec::new();
        let mut writes = Vec::new();
        let mut halts = 0;
        let mut aligns = 0;

        while dma.is_busy() {
            cycles += 1;
            assert!(cycles <= 520, "OAM DMA did not finish");

            if Some(cycles) == stolen_cycle {
                assert_eq!(phase, DmaPhase::Get);
                // Another bus master owns this cycle. Do NOT advance OAM,
                // but elapsed time and the global Get/Put phase still advance.
                phase = next_phase(phase);
                continue;
            }

            match dma.clock(phase, CpuBusCycle::Read).unwrap() {
                OamDmaAction::Pass => panic!("active DMA unexpectedly returned Pass"),
                OamDmaAction::Halt => halts += 1,
                OamDmaAction::Align => aligns += 1,
                OamDmaAction::Read(addr) => {
                    assert_eq!(phase, DmaPhase::Get);
                    assert_eq!(reads.len(), writes.len());
                    assert_eq!(addr, 0x0200 + reads.len() as u16);
                    reads.push(addr);
                    assert!(dma.is_busy());
                    dma.complete_read(source_byte(addr as u8)).unwrap();
                }
                OamDmaAction::Write(value) => {
                    assert_eq!(phase, DmaPhase::Put);
                    assert_eq!(reads.len(), writes.len() + 1);
                    assert_eq!(value, source_byte(writes.len() as u8));
                    writes.push(value);
                    assert_eq!(dma.is_busy(), writes.len() < 256);
                }
            }
            phase = next_phase(phase);
        }

        assert_eq!(reads.len(), 256);
        assert_eq!(writes.len(), 256);
        assert_eq!(reads.first(), Some(&0x0200));
        assert_eq!(reads.last(), Some(&0x02FF));
        assert_eq!(halts, 1);
        let initial_align = usize::from(first_phase == DmaPhase::Get);
        assert_eq!(aligns, initial_align + usize::from(stolen_cycle.is_some()));
        assert_eq!(
            dma.clock(phase, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Pass
        );
        cycles
    }

    #[test]
    fn idle_passes_both_cpu_access_types_on_both_phases() {
        let mut dma = OamDma::new();
        assert!(!dma.is_busy());
        for phase in [DmaPhase::Get, DmaPhase::Put] {
            for access in [CpuBusCycle::Read, CpuBusCycle::Write] {
                assert_eq!(dma.clock(phase, access).unwrap(), OamDmaAction::Pass);
                assert!(!dma.is_busy());
            }
        }
    }

    #[test]
    fn transfer_starting_with_put_halt_takes_513_cycles() {
        assert_eq!(transfer(DmaPhase::Put, None), 513);
    }

    #[test]
    fn transfer_starting_with_get_halt_takes_514_cycles() {
        assert_eq!(transfer(DmaPhase::Get, None), 514);
    }

    #[test]
    fn pending_request_does_not_halt_cpu_writes_and_latest_page_wins() {
        let mut dma = OamDma::new();
        dma.request(0x02).unwrap();
        assert!(dma.is_busy());
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Write).unwrap(),
            OamDmaAction::Pass
        );
        // Simulate a second write to $4014 before the CPU can be halted.
        dma.request(0x03).unwrap();
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Write).unwrap(),
            OamDmaAction::Pass
        );
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Halt
        );
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Read(0x0300)
        );
    }

    #[test]
    fn active_request_is_rejected_without_replacing_the_page() {
        let mut dma = OamDma::new();
        dma.request(0x02).unwrap();
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Halt
        );
        assert!(dma.request(0x03).is_err());
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Read(0x0200)
        );
        assert!(dma.request(0x04).is_err());
        dma.complete_read(0xA5).unwrap();
        assert!(dma.request(0x05).is_err());
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Write(0xA5)
        );
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Read(0x0201)
        );
    }

    #[test]
    fn read_completion_is_required_once_and_does_not_consume_a_cycle() {
        let mut dma = OamDma::new();
        assert!(dma.complete_read(0x11).is_err());
        dma.request(0x02).unwrap();
        assert!(dma.complete_read(0x22).is_err());
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Halt
        );
        assert!(dma.complete_read(0x33).is_err());
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Read(0x0200)
        );
        // This is deliberately invalid use; report the missing completion
        // without mutating state. It is not an elapsed hardware cycle.
        assert!(dma.clock(DmaPhase::Put, CpuBusCycle::Read).is_err());
        dma.complete_read(0xA5).unwrap();
        assert!(dma.complete_read(0x5A).is_err());
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Write(0xA5)
        );
    }

    #[test]
    fn losing_one_get_slot_preserves_offset_and_does_not_write_stale_data() {
        let mut dma = OamDma::new();
        dma.request(0x02).unwrap();
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Halt
        );
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Read(0x0200)
        );
        dma.complete_read(0xA5).unwrap();
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Write(0xA5)
        );
        // The following Get is stolen: no call to OamDma::clock on that cycle.
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Align
        );
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Read(0x0201)
        );
        dma.complete_read(0x3C).unwrap();
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Write(0x3C)
        );
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Read(0x0202)
        );
    }

    #[test]
    fn complete_transfer_with_one_stolen_get_takes_two_extra_cycles() {
        assert_eq!(transfer(DmaPhase::Put, Some(4)), 515);
        assert_eq!(transfer(DmaPhase::Get, Some(5)), 516);
    }

    #[test]
    fn page_ff_reaches_ffff_without_wrapping_into_an_extra_transfer() {
        let mut dma = OamDma::new();
        dma.request(0xFF).unwrap();
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Halt
        );
        for offset in 0..256u16 {
            assert_eq!(
                dma.clock(DmaPhase::Get, CpuBusCycle::Read).unwrap(),
                OamDmaAction::Read(0xFF00 | offset)
            );
            dma.complete_read(offset as u8).unwrap();
            assert_eq!(
                dma.clock(DmaPhase::Put, CpuBusCycle::Read).unwrap(),
                OamDmaAction::Write(offset as u8)
            );
        }
        assert!(!dma.is_busy());
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Pass
        );

        dma.request(0x00).unwrap();
        assert_eq!(
            dma.clock(DmaPhase::Put, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Halt
        );
        assert_eq!(
            dma.clock(DmaPhase::Get, CpuBusCycle::Read).unwrap(),
            OamDmaAction::Read(0x0000)
        );
    }
}
