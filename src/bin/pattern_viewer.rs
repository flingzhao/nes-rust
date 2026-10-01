use nes::{cartridge::Cartridge, debug_video::build_pattern_tables};

use sdl2::{event::Event, keyboard::Keycode, pixels::PixelFormatEnum};
use std::time::Duration;

fn show_pattern_tables(rgb: &[u8]) -> Result<(), String> {
    const WIDTH: usize = 256;
    const HEIGHT: usize = 128;

    if rgb.len() != WIDTH * HEIGHT * 3 {
        return Err("Invalid pattern-table RGB buffer length".to_string());
    }

    // 使用最近邻缩放，保持像素边缘清晰。
    sdl2::hint::set("SDL_RENDER_SCALE_QUALITY", "0");

    let sdl = sdl2::init()?;
    let video = sdl.video()?;

    let window = video
        .window("NES Pattern Tables", 768, 384)
        .position_centered()
        .build()
        .map_err(|e| e.to_string())?;

    let mut canvas = window.into_canvas().build().map_err(|e| e.to_string())?;

    canvas
        .set_logical_size(WIDTH as u32, HEIGHT as u32)
        .map_err(|e| e.to_string())?;

    let texture_creator = canvas.texture_creator();
    let mut texture = texture_creator
        .create_texture_streaming(PixelFormatEnum::RGB24, WIDTH as u32, HEIGHT as u32)
        .map_err(|e| e.to_string())?;

    texture
        .update(None, rgb, WIDTH * 3)
        .map_err(|e| e.to_string())?;

    let mut events = sdl.event_pump()?;

    'running: loop {
        for event in events.poll_iter() {
            match event {
                Event::Quit { .. }
                | Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => break 'running,
                _ => {}
            }
        }

        canvas.clear();
        canvas.copy(&texture, None, None)?;
        canvas.present();

        // 当前只是静态查看器，避免空转。
        // 这不是未来模拟器的音画同步方案。
        std::thread::sleep(Duration::from_millis(16));
    }

    Ok(())
}

fn main() -> Result<(), String> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or_else(|| "Usage: pattern-viewer <rom.nes>".to_string())?;

    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let cartridge = Cartridge::try_from(bytes.as_slice())?;
    let rgb = build_pattern_tables(&cartridge)?;

    show_pattern_tables(&rgb)
}
