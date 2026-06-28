use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use image::imageops::FilterType;
use std::io::{self, Read, Write};
use std::process::Command;
use std::thread;
use std::time::Duration;

const FRAME_COUNT: usize = 4;
const DISPLAY_HEIGHT: u32 = 120;
const FRAME_DELAY: Duration = Duration::from_millis(50);
const STEP_COLS: usize = 2;

fn encode_png(img: &image::RgbaImage) -> Vec<u8> {
    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    image::ImageEncoder::write_image(
        encoder,
        img.as_raw(),
        img.width(),
        img.height(),
        image::ColorType::Rgba8.into(),
    )
    .expect("failed to encode PNG");
    buf
}

fn terminal_size() -> (usize, usize, usize, usize) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_xpixel > 0 {
            return (
                ws.ws_col as usize,
                ws.ws_row as usize,
                ws.ws_xpixel as usize,
                ws.ws_ypixel as usize,
            );
        }
    }
    (80, 24, 800, 480)
}

fn kitty_transmit_chunked(out: &mut impl Write, image_id: u32, png_data: &[u8]) {
    let encoded = BASE64.encode(png_data);
    let chunks: Vec<&[u8]> = encoded.as_bytes().chunks(4096).collect();
    let last = chunks.len() - 1;
    for (i, chunk) in chunks.iter().enumerate() {
        let more = if i < last { 1 } else { 0 };
        let chunk_str = std::str::from_utf8(chunk).unwrap();
        if i == 0 {
            write!(
                out,
                "\x1b_Gi={},f=100,a=t,t=d,q=2,m={};{}\x1b\\",
                image_id, more, chunk_str
            )
            .unwrap();
        } else {
            write!(out, "\x1b_Gm={};{}\x1b\\", more, chunk_str).unwrap();
        }
    }
}

fn kitty_place(out: &mut impl Write, image_id: u32, display_cols: usize, display_rows: usize) {
    write!(
        out,
        "\x1b_Ga=p,i={},p=1,c={},r={},C=1,q=2\x1b\\",
        image_id, display_cols, display_rows
    )
    .unwrap();
}

fn kitty_delete_placement(out: &mut impl Write, image_id: u32) {
    write!(out, "\x1b_Ga=d,d=i,i={},q=2\x1b\\", image_id).unwrap();
}

fn kitty_free_image(out: &mut impl Write, image_id: u32) {
    write!(out, "\x1b_Ga=d,d=I,i={},q=2\x1b\\", image_id).unwrap();
}

fn query_cursor_row() -> usize {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // Set terminal to raw mode to read the response
    let fd = 0; // stdin
    let mut orig_termios: libc::termios = unsafe { std::mem::zeroed() };
    unsafe { libc::tcgetattr(fd, &mut orig_termios) };
    let mut raw = orig_termios;
    unsafe { libc::cfmakeraw(&mut raw) };
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) };

    // Send CPR (Cursor Position Report) request
    write!(out, "\x1b[6n").unwrap();
    out.flush().unwrap();

    // Read response: \x1b[row;colR
    let mut buf = Vec::new();
    let mut reader = stdin.lock();
    loop {
        let mut byte = [0u8; 1];
        if reader.read(&mut byte).unwrap_or(0) == 0 {
            break;
        }
        buf.push(byte[0]);
        if byte[0] == b'R' {
            break;
        }
    }

    // Restore terminal mode
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &orig_termios) };

    // Parse \x1b[row;colR
    let s = String::from_utf8_lossy(&buf);
    if let Some(start) = s.find('[') {
        let inner = &s[start + 1..s.len() - 1]; // strip '[' and 'R'
        if let Some(semi) = inner.find(';') {
            if let Ok(row) = inner[..semi].parse::<usize>() {
                return row;
            }
        }
    }
    // fallback
    10
}

fn main() {
    let raw_frames: [&[u8]; FRAME_COUNT] = [
        include_bytes!("../assets/frame1.png"),
        include_bytes!("../assets/frame2.png"),
        include_bytes!("../assets/frame3.png"),
        include_bytes!("../assets/frame4.png"),
    ];

    let (term_cols, _term_rows, term_w_px, term_h_px) = terminal_size();
    let cell_w_px = term_w_px as f64 / term_cols as f64;
    let cell_h_px = term_h_px as f64 / _term_rows as f64;

    let first_img = image::load_from_memory(raw_frames[0]).unwrap();
    let aspect = first_img.width() as f64 / first_img.height() as f64;
    let display_width = (DISPLAY_HEIGHT as f64 * aspect) as u32;

    let car_cols = (display_width as f64 / cell_w_px).ceil() as usize;
    let car_rows = (DISPLAY_HEIGHT as f64 / cell_h_px).ceil() as usize;

    let stdout = io::stdout();
    let mut out = stdout.lock();

    // Resize and upload all frames
    let mut png_frames = Vec::new();
    for (i, raw) in raw_frames.iter().enumerate() {
        let img = image::load_from_memory(raw).unwrap();
        let resized = image::imageops::resize(
            &img.to_rgba8(),
            display_width,
            DISPLAY_HEIGHT,
            FilterType::Lanczos3,
        );
        let png = encode_png(&resized);
        let image_id = (i + 1) as u32;
        kitty_transmit_chunked(&mut out, image_id, &png);
        png_frames.push(image_id);
    }
    out.flush().unwrap();

    // Reserve vertical space for the car
    for _ in 0..car_rows {
        writeln!(out).unwrap();
    }
    out.flush().unwrap();

    // Query current cursor row via DSR (Device Status Report)
    // The cursor is now at the bottom of the reserved space.
    // The car's top-left row is (current_row - car_rows + 1).
    let anchor_row = query_cursor_row() - car_rows + 1;

    // Hide cursor
    write!(out, "\x1b[?25l").unwrap();
    out.flush().unwrap();

    // Animate: car moves from left to right
    let total_steps = (term_cols + car_cols) / STEP_COLS;
    let mut prev_image_id: Option<u32> = None;

    for step in 0..total_steps {
        let col = step as isize * STEP_COLS as isize - car_cols as isize + 1;

        // Delete previous placement
        if let Some(prev_id) = prev_image_id {
            kitty_delete_placement(&mut out, prev_id);
        }

        let frame_id = png_frames[step % FRAME_COUNT];

        // Move cursor to anchor row with absolute positioning
        let display_col = col.max(1) as usize;
        write!(out, "\x1b[{};{}H", anchor_row, display_col).unwrap();

        kitty_place(&mut out, frame_id, car_cols, car_rows);

        // Move cursor to the row just after the car area
        write!(out, "\x1b[{};1H", anchor_row + car_rows).unwrap();

        out.flush().unwrap();
        prev_image_id = Some(frame_id);

        thread::sleep(FRAME_DELAY);
    }

    // Cleanup: delete all placements and free image data
    if let Some(prev_id) = prev_image_id {
        kitty_delete_placement(&mut out, prev_id);
    }
    for id in &png_frames {
        kitty_free_image(&mut out, *id);
    }

    // Show cursor
    write!(out, "\x1b[?25h").unwrap();
    out.flush().unwrap();

    // Run git
    let args: Vec<String> = std::env::args().skip(1).collect();
    let status = Command::new("git")
        .args(&args)
        .status()
        .expect("failed to execute git");
    std::process::exit(status.code().unwrap_or(1));
}
