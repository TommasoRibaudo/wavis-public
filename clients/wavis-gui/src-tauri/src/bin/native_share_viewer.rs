#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("native_share_viewer is only available on Linux");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
use std::io::{Read, Write};
#[cfg(target_os = "linux")]
use std::net::TcpStream;
#[cfg(target_os = "linux")]
use std::cell::RefCell;
#[cfg(target_os = "linux")]
use std::rc::Rc;
#[cfg(target_os = "linux")]
use std::sync::mpsc;
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use gdk_pixbuf::{Colorspace, PixbufLoader};
#[cfg(target_os = "linux")]
use gtk::glib::translate::ToGlibPtr;
#[cfg(target_os = "linux")]
use gtk::prelude::*;

#[cfg(target_os = "linux")]
fn main() {
    let mut args = std::env::args().skip(1);
    let Some(url) = args.next() else {
        eprintln!("native_share_viewer: missing stream URL");
        std::process::exit(2);
    };
    let title = args.next().unwrap_or_else(|| "Wavis screen share".to_string());

    gtk::init().expect("native_share_viewer: GTK init failed");

    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_title(&title);
    window.set_default_size(960, 540);
    window.set_resizable(true);

    let latest_pixbuf: Rc<RefCell<Option<gdk_pixbuf::Pixbuf>>> = Rc::new(RefCell::new(None));
    let drawing_area = gtk::DrawingArea::new();
    drawing_area.set_hexpand(true);
    drawing_area.set_vexpand(true);
    window.add(&drawing_area);

    {
        let latest_pixbuf = Rc::clone(&latest_pixbuf);
        drawing_area.connect_draw(move |area, cr| {
            let allocation = area.allocation();
            let area_w = f64::from(allocation.width()).max(1.0);
            let area_h = f64::from(allocation.height()).max(1.0);

            cr.set_source_rgb(0.11, 0.13, 0.16);
            let _ = cr.paint();

            let Some(pixbuf) = latest_pixbuf.borrow().clone() else {
                return false.into();
            };
            let frame_w = f64::from(pixbuf.width()).max(1.0);
            let frame_h = f64::from(pixbuf.height()).max(1.0);
            let scale = (area_w / frame_w).min(area_h / frame_h);
            let draw_w = frame_w * scale;
            let draw_h = frame_h * scale;
            let x = (area_w - draw_w) / 2.0;
            let y = (area_h - draw_h) / 2.0;

            let _ = cr.save();
            cr.translate(x, y);
            cr.scale(scale, scale);
            unsafe {
                gdk::ffi::gdk_cairo_set_source_pixbuf(
                    cr.to_glib_none().0,
                    pixbuf.to_glib_none().0,
                    0.0,
                    0.0,
                );
            }
            cr.rectangle(0.0, 0.0, frame_w, frame_h);
            let _ = cr.fill();
            let _ = cr.restore();
            false.into()
        });
    }
    window.show_all();
    window.connect_destroy(|_| {
        gtk::main_quit();
    });

    let (tx, rx) = mpsc::channel::<ViewerFrame>();
    std::thread::Builder::new()
        .name("native-share-viewer-stream".into())
        .spawn(move || {
            if let Err(err) = stream_frames(&url, tx) {
                eprintln!("native_share_viewer: stream ended: {err}");
            }
        })
        .expect("native_share_viewer: failed to spawn stream thread");

    gtk::glib::timeout_add_local(Duration::from_millis(1), move || {
        let mut latest = None;
        while let Ok(bytes) = rx.try_recv() {
            latest = Some(bytes);
        }
        if let Some(frame) = latest {
            if let Some(pixbuf) = frame.into_pixbuf() {
                *latest_pixbuf.borrow_mut() = Some(pixbuf);
                drawing_area.queue_draw();
            }
        }
        gtk::glib::ControlFlow::Continue
    });

    gtk::main();
}

#[cfg(target_os = "linux")]
struct ViewerFrame {
    width: u32,
    height: u32,
    data: ViewerFrameData,
}

#[cfg(target_os = "linux")]
enum ViewerFrameData {
    Jpeg(Vec<u8>),
    Rgba(Vec<u8>),
}

#[cfg(target_os = "linux")]
impl ViewerFrame {
    fn into_pixbuf(self) -> Option<gdk_pixbuf::Pixbuf> {
        match self.data {
            ViewerFrameData::Jpeg(bytes) => decode_jpeg(&bytes),
            ViewerFrameData::Rgba(bytes) => {
                if self.width == 0 || self.height == 0 {
                    return None;
                }
                let expected_len = self.width as usize * self.height as usize * 4;
                if bytes.len() != expected_len {
                    return None;
                }
                Some(gdk_pixbuf::Pixbuf::from_mut_slice(
                    bytes,
                    Colorspace::Rgb,
                    true,
                    8,
                    self.width as i32,
                    self.height as i32,
                    self.width as i32 * 4,
                ))
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn stream_frames(url: &str, tx: mpsc::Sender<ViewerFrame>) -> Result<(), String> {
    let parsed = parse_local_url(url)?;
    let mut stream = TcpStream::connect(("127.0.0.1", parsed.port))
        .map_err(|e| format!("connect failed: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|e| format!("set_read_timeout failed: {e}"))?;

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        parsed.target, parsed.port
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("request write failed: {e}"))?;

    let mut buffer = Vec::<u8>::with_capacity(512 * 1024);
    read_until_headers(&mut stream, &mut buffer)?;
    if parsed.raw {
        loop {
            let frame = read_raw_frame(&mut stream, &mut buffer)?;
            if tx.send(frame).is_err() {
                return Ok(());
            }
        }
    }
    loop {
        let frame = ViewerFrame {
            width: 0,
            height: 0,
            data: ViewerFrameData::Jpeg(read_multipart_frame(&mut stream, &mut buffer)?),
        };
        if tx.send(frame).is_err() {
            return Ok(());
        }
    }
}

#[cfg(target_os = "linux")]
struct ParsedUrl {
    port: u16,
    target: String,
    raw: bool,
}

#[cfg(target_os = "linux")]
fn parse_local_url(url: &str) -> Result<ParsedUrl, String> {
    let rest = url
        .strip_prefix("http://127.0.0.1:")
        .ok_or_else(|| "only local 127.0.0.1 URLs are supported".to_string())?;
    let (port_str, target) = rest
        .split_once('/')
        .ok_or_else(|| "URL missing path".to_string())?;
    let port = port_str
        .parse::<u16>()
        .map_err(|e| format!("invalid port: {e}"))?;
    Ok(ParsedUrl {
        port,
        raw: target.contains("format=raw"),
        target: format!("/{target}"),
    })
}

#[cfg(target_os = "linux")]
fn read_raw_frame(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Result<ViewerFrame, String> {
    while buffer.len() < 20 {
        read_more(stream, buffer)?;
    }
    let width = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]);
    let height = u32::from_le_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]);
    let _seq = u64::from_le_bytes([
        buffer[8], buffer[9], buffer[10], buffer[11], buffer[12], buffer[13], buffer[14],
        buffer[15],
    ]);
    let len = u32::from_le_bytes([buffer[16], buffer[17], buffer[18], buffer[19]]) as usize;
    buffer.drain(..20);

    let expected_len = width as usize * height as usize * 4;
    if width == 0 || height == 0 || len != expected_len || len > 100_000_000 {
        return Err(format!("invalid raw frame header: {width}x{height} len={len}"));
    }

    while buffer.len() < len {
        read_more(stream, buffer)?;
    }

    Ok(ViewerFrame {
        width,
        height,
        data: ViewerFrameData::Rgba(buffer.drain(..len).collect()),
    })
}

#[cfg(target_os = "linux")]
fn read_until_headers(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Result<(), String> {
    loop {
        if let Some(end) = find_header_end(buffer) {
            buffer.drain(..end + 4);
            return Ok(());
        }
        read_more(stream, buffer)?;
    }
}

#[cfg(target_os = "linux")]
fn read_multipart_frame(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Result<Vec<u8>, String> {
    loop {
        if let Some(boundary) = find_bytes(buffer, b"--wavisframe") {
            buffer.drain(..boundary + "--wavisframe".len());
            break;
        }
        read_more(stream, buffer)?;
    }

    let header_end = loop {
        if let Some(end) = find_header_end(buffer) {
            break end;
        }
        read_more(stream, buffer)?;
    };

    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let content_len = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .ok_or_else(|| "multipart frame missing content-length".to_string())?;

    buffer.drain(..header_end + 4);
    while buffer.len() < content_len + 2 {
        read_more(stream, buffer)?;
    }
    let frame = buffer.drain(..content_len).collect::<Vec<_>>();
    if buffer.starts_with(b"\r\n") {
        buffer.drain(..2);
    }
    Ok(frame)
}

#[cfg(target_os = "linux")]
fn read_more(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Result<(), String> {
    let mut chunk = [0u8; 64 * 1024];
    let read = stream
        .read(&mut chunk)
        .map_err(|e| format!("read failed: {e}"))?;
    if read == 0 {
        return Err("stream closed".to_string());
    }
    buffer.extend_from_slice(&chunk[..read]);
    Ok(())
}

#[cfg(target_os = "linux")]
fn find_header_end(bytes: &[u8]) -> Option<usize> {
    find_bytes(bytes, b"\r\n\r\n")
}

#[cfg(target_os = "linux")]
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(target_os = "linux")]
fn decode_jpeg(bytes: &[u8]) -> Option<gdk_pixbuf::Pixbuf> {
    let loader = PixbufLoader::new();
    loader.write(bytes).ok()?;
    loader.close().ok()?;
    loader.pixbuf()
}
