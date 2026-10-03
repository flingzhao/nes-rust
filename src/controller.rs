#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Button {
    A = 0x01,
    B = 0x02,
    Select = 0x04,
    Start = 0x08,
    Up = 0x10,
    Down = 0x20,
    Left = 0x40,
    Right = 0x80,
}

pub struct Controller {
    buttons: u8,
    latched: u8,
    read_index: u8,
    strobe: bool,
}

impl Controller {
    pub fn new() -> Self {
        Self {
            buttons: 0,
            latched: 0,
            read_index: 0,
            strobe: false,
        }
    }

    pub fn set_button(&mut self, button: Button, pressed: bool) {
        let mask = button as u8;

        if pressed {
            self.buttons |= mask;
        } else {
            self.buttons &= !mask;
        }
    }

    pub fn release_all(&mut self) {
        self.buttons = 0;
    }

    pub fn write_strobe(&mut self, value: u8) {
        let new_strobe = value & 1 != 0;

        // 从高电平变为低电平：保存按钮快照，从 A 开始读取。
        if self.strobe && !new_strobe {
            self.latched = self.buttons;
            self.read_index = 0;
        }

        // 高电平期间始终停留在第一个按钮。
        if new_strobe {
            self.read_index = 0;
        }

        self.strobe = new_strobe;
    }

    pub fn read(&mut self) -> u8 {
        // 高电平期间返回实时 A 状态，不推进。
        if self.strobe {
            return self.buttons & 1;
        }

        // 标准手柄读完 8 个按钮后，持续返回 1。
        if self.read_index >= 8 {
            return 1;
        }

        let value = (self.latched >> self.read_index) & 1;
        self.read_index += 1;
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latch(controller: &mut Controller) {
        controller.write_strobe(1);
        controller.write_strobe(0);
    }

    fn read_eight(controller: &mut Controller) -> [u8; 8] {
        std::array::from_fn(|_| controller.read())
    }

    #[test]
    fn every_button_has_the_correct_serial_position() {
        let buttons = [
            Button::A,
            Button::B,
            Button::Select,
            Button::Start,
            Button::Up,
            Button::Down,
            Button::Left,
            Button::Right,
        ];

        for (position, button) in buttons.into_iter().enumerate() {
            let mut controller = Controller::new();
            controller.set_button(button, true);
            latch(&mut controller);

            let mut expected = [0; 8];
            expected[position] = 1;
            assert_eq!(read_eight(&mut controller), expected);
        }
    }

    #[test]
    fn supports_multiple_buttons_and_returns_one_after_eight_reads() {
        let mut controller = Controller::new();
        controller.set_button(Button::A, true);
        controller.set_button(Button::Right, true);
        latch(&mut controller);

        assert_eq!(read_eight(&mut controller), [1, 0, 0, 0, 0, 0, 0, 1]);

        for _ in 0..300 {
            assert_eq!(controller.read(), 1);
        }
    }

    #[test]
    fn high_strobe_reads_live_a_without_advancing() {
        let mut controller = Controller::new();

        controller.write_strobe(0x81); // 只看 bit 0。
        controller.set_button(Button::A, true);

        for _ in 0..10 {
            assert_eq!(controller.read(), 1);
        }

        controller.set_button(Button::A, false);
        controller.set_button(Button::B, true);

        for _ in 0..10 {
            assert_eq!(controller.read(), 0);
        }

        controller.write_strobe(0x80); // bit 0 从 1 变成 0。
        assert_eq!(read_eight(&mut controller), [0, 1, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn button_changes_do_not_modify_an_existing_snapshot() {
        let mut controller = Controller::new();
        controller.set_button(Button::A, true);
        latch(&mut controller);

        controller.set_button(Button::A, false);
        controller.set_button(Button::B, true);

        // 当前快照仍然是 A 按下。
        assert_eq!(read_eight(&mut controller), [1, 0, 0, 0, 0, 0, 0, 0]);

        // 下一次锁存才采集新的 B。
        latch(&mut controller);
        assert_eq!(read_eight(&mut controller), [0, 1, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn repeated_low_writes_do_not_restart_reading() {
        let mut controller = Controller::new();
        controller.set_button(Button::A, true);
        latch(&mut controller);

        assert_eq!(controller.read(), 1); // A

        controller.write_strobe(0);
        assert_eq!(controller.read(), 0); // 必须是 B，而不是重新读 A。
    }

    #[test]
    fn release_all_clears_live_buttons_but_preserves_snapshot() {
        let mut controller = Controller::new();
        controller.set_button(Button::A, true);
        controller.set_button(Button::Right, true);
        latch(&mut controller);

        controller.release_all();

        assert_eq!(read_eight(&mut controller), [1, 0, 0, 0, 0, 0, 0, 1]);

        latch(&mut controller);
        assert_eq!(read_eight(&mut controller), [0; 8]);
    }
}
