use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::pixels::PixelFormatEnum;
use std::time::{Duration, Instant};

use nes::bus::Bus;
use nes::cartridge::Cartridge;
use nes::controller::Button;
use nes::cpu::Cpu;
use nes::nes::Nes;

const DISPLAY_PALETTE: [u32; 64] = [
    0x808080, 0x003DA6, 0x0012B0, 0x440096, 0xA1005E, 0xC70028, 0xBA0600, 0x8C1700, 0x5C2F00,
    0x104500, 0x054A00, 0x00472E, 0x004166, 0x000000, 0x050505, 0x050505, 0xC7C7C7, 0x0077FF,
    0x2155FF, 0x8237FA, 0xEB2FB5, 0xFF2950, 0xFF2200, 0xD63200, 0xC46200, 0x358000, 0x058F00,
    0x008A55, 0x0099CC, 0x212121, 0x090909, 0x090909, 0xFFFFFF, 0x0FD7FF, 0x69A2FF, 0xD480FF,
    0xFF45F3, 0xFF618B, 0xFF8833, 0xFF9C12, 0xFABC20, 0x9FE30E, 0x2BF035, 0x0CF0A4, 0x05FBFF,
    0x5E5E5E, 0x0D0D0D, 0x0D0D0D, 0xFFFFFF, 0xA6FCFF, 0xB3ECFF, 0xDAABEB, 0xFFA8F9, 0xFFABB3,
    0xFFD2B0, 0xFFEFA6, 0xFFF79C, 0xD7E895, 0xA6EDAF, 0xA2F2DA, 0x99FFFC, 0xDDDDDD, 0x111111,
    0x111111,
];

fn indices_to_rgb(indices: &[u8], rgb: &mut [u8]) -> Result<(), String> {
    if indices.len() != 256 * 240 {
        return Err(format!(
            "Expected 61440 framebuffer pixels, got {}",
            indices.len()
        ));
    }

    if rgb.len() != 256 * 240 * 3 {
        return Err(format!("Expected 184320 RGB bytes, got {}", rgb.len()));
    }

    for (position, (&index, output)) in indices.iter().zip(rgb.chunks_exact_mut(3)).enumerate() {
        let color = *DISPLAY_PALETTE
            .get(usize::from(index))
            .ok_or_else(|| format!("Invalid color index {index} at pixel {position}"))?;

        output[0] = (color >> 16) as u8;
        output[1] = (color >> 8) as u8;
        output[2] = color as u8;
    }

    Ok(())
}

fn map_button(key: Keycode) -> Option<Button> {
    match key {
        Keycode::Z => Some(Button::A),
        Keycode::X => Some(Button::B),
        Keycode::RShift => Some(Button::Select),
        Keycode::Return => Some(Button::Start),
        Keycode::Up => Some(Button::Up),
        Keycode::Down => Some(Button::Down),
        Keycode::Left => Some(Button::Left),
        Keycode::Right => Some(Button::Right),
        _ => None,
    }
}

fn run_window(mut nes: Nes) -> Result<(), String> {
    let sdl = sdl2::init()?;
    let video = sdl.video()?;

    // 使用整数倍窗口；不使用线性过滤模糊像素。
    sdl2::hint::set("SDL_RENDER_SCALE_QUALITY", "0");

    let window = video
        .window("NES", 256 * 3, 240 * 3)
        .position_centered()
        .build()
        .map_err(|error| error.to_string())?;

    // 本轮用软件限速，不同时依赖显示器垂直同步。
    let mut canvas = window
        .into_canvas()
        .build()
        .map_err(|error| error.to_string())?;

    canvas
        .set_logical_size(256, 240)
        .map_err(|error| error.to_string())?;

    let texture_creator = canvas.texture_creator();
    let mut texture = texture_creator
        .create_texture_streaming(PixelFormatEnum::RGB24, 256, 240)
        .map_err(|error| error.to_string())?;

    let mut events = sdl.event_pump()?;
    let mut rgb = vec![0u8; 256 * 240 * 3];

    // 临时显示限速；后续音频阶段再统一处理同步策略。
    let frame_duration = Duration::from_secs_f64(1.0 / 60.0988);

    'running: loop {
        let started = Instant::now();

        for event in events.poll_iter() {
            use sdl2::event::WindowEvent;

            match event {
                Event::Quit { .. }
                | Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => break 'running,

                Event::KeyDown {
                    keycode: Some(key),
                    repeat: false,
                    ..
                } => {
                    if let Some(button) = map_button(key) {
                        nes.set_controller_button(0, button, true)?;
                    }
                }

                Event::KeyUp {
                    keycode: Some(key), ..
                } => {
                    if let Some(button) = map_button(key) {
                        nes.set_controller_button(0, button, false)?;
                    }
                }

                Event::Window {
                    win_event: WindowEvent::FocusLost,
                    ..
                } => {
                    nes.release_all_buttons();
                }

                _ => {}
            }
        }

        // nes.run_until_frame()?;
        if let Err(error) = nes.run_until_frame() {
            eprintln!(
                "NES execution failed at CPU cycle {}: {}",
                nes.cpu_cycles(),
                error
            );
            return Err(error);
        }
        indices_to_rgb(nes.framebuffer(), &mut rgb)?;

        texture
            .update(None, &rgb, 256 * 3)
            .map_err(|error| error.to_string())?;

        canvas.clear();
        canvas.copy(&texture, None, None)?;
        canvas.present();

        if let Some(remaining) = frame_duration.checked_sub(started.elapsed()) {
            std::thread::sleep(remaining);
        }
    }

    Ok(())
}

fn main() -> Result<(), String> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or_else(|| "Usage: pattern-viewer <rom.nes>".to_string())?;

    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let cartridge = Cartridge::try_from(bytes.as_slice())?;

    let mut bus = Bus::new();
    bus.insert_cartridge(cartridge);

    let mut cpu = Cpu::new();
    cpu.reset(&mut bus)?;

    let mut nes = Nes::new(cpu, bus);
    run_window(nes);

    Ok(())
}
