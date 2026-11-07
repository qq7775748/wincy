//! Raw Win32 clipboard access: read every useful format, filter out items that
//! applications mark as confidential (same idea as Maccy's "concealed" types),
//! and write formats back so rich text / images / files paste natively.

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardFormatNameW,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
    SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;

/// Formats that Windows apps use to say "do not keep this in a history".
const HIDDEN_FORMATS: [&str; 3] = [
    "ExcludeClipboardContentFromMonitorProcessing",
    "Clipboard Viewer Ignore",
    "CanIncludeInClipboardHistory",
];

#[derive(Debug, Clone, Default)]
pub struct Clip {
    pub kind: String, // text | rich | image | files
    pub text: String,
    pub html: Option<String>,
    pub png: Option<Vec<u8>>,
    pub files: Vec<String>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn fmt_id(name: &str) -> u32 {
    let w = wide(name);
    unsafe { RegisterClipboardFormatW(PCWSTR(w.as_ptr())) }
}

pub fn sequence() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

fn open(retries: u32) -> bool {
    for _ in 0..retries {
        if unsafe { OpenClipboard(None) }.is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(12));
    }
    false
}

unsafe fn format_name(fmt: u32) -> String {
    let mut buf = [0u16; 256];
    let len = GetClipboardFormatNameW(fmt, &mut buf);
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

unsafe fn bytes(fmt: u32) -> Option<Vec<u8>> {
    let h = GetClipboardData(fmt).ok()?;
    let hg = HGLOBAL(h.0);
    let ptr = GlobalLock(hg) as *const u8;
    if ptr.is_null() {
        return None;
    }
    // CF_HDROP / CF_DIB / text blobs all live in movable global memory, so
    // GlobalSize gives us a safe upper bound.
    let size = GlobalSize(hg);
    let out = if size == 0 {
        None
    } else {
        Some(std::slice::from_raw_parts(ptr, size).to_vec())
    };
    let _ = GlobalUnlock(hg);
    out
}

unsafe fn excluded() -> bool {
    let mut fmt = 0u32;
    loop {
        fmt = EnumClipboardFormats(fmt);
        if fmt == 0 {
            return false;
        }
        let name = format_name(fmt);
        if name.is_empty() {
            continue;
        }
        for hidden in HIDDEN_FORMATS {
            if name.eq_ignore_ascii_case(hidden) {
                if hidden == "CanIncludeInClipboardHistory" {
                    // Value 0 => "do not keep"; missing value is treated as keep.
                    if let Some(b) = bytes(fmt) {
                        if b.len() >= 4 && u32::from_le_bytes([b[0], b[1], b[2], b[3]]) == 0 {
                            return true;
                        }
                    }
                } else {
                    return true;
                }
            }
        }
    }
}

unsafe fn read_text(fmt: u32) -> Option<String> {
    let b = bytes(fmt)?;
    if b.len() < 2 {
        return None;
    }
    let units: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|u| *u != 0)
        .collect();
    if units.is_empty() {
        return None;
    }
    Some(String::from_utf16_lossy(&units))
}

unsafe fn read_files() -> Vec<String> {
    let mut out = Vec::new();
    if IsClipboardFormatAvailable(CF_HDROP).is_err() {
        return out;
    }
    let Ok(h) = GetClipboardData(CF_HDROP) else {
        return out;
    };
    let hdrop = HDROP(h.0);
    let count = DragQueryFileW(hdrop, u32::MAX, None);
    for i in 0..count {
        let len = DragQueryFileW(hdrop, i, None) as usize;
        if len == 0 {
            continue;
        }
        let mut buf = vec![0u16; len + 1];
        let n = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
        if n > 0 {
            out.push(String::from_utf16_lossy(&buf[..n]));
        }
    }
    out
}

/// HTML on the Windows clipboard is a CF_HTML blob with byte offsets in a
/// header. We only need the fragment between the markers.
fn html_fragment(raw: &str) -> String {
    if let (Some(s), Some(e)) = (raw.find("<!--StartFragment-->"), raw.rfind("<!--EndFragment-->")) {
        if e > s {
            return raw[s + "<!--StartFragment-->".len()..e].to_string();
        }
    }
    raw.to_string()
}

unsafe fn read_html() -> Option<String> {
    let id = fmt_id("HTML Format");
    if id == 0 || IsClipboardFormatAvailable(id).is_err() {
        return None;
    }
    let b = bytes(id)?;
    let raw = String::from_utf8_lossy(&b).to_string();
    let frag = html_fragment(&raw).trim().to_string();
    if frag.is_empty() {
        None
    } else {
        Some(frag)
    }
}

unsafe fn read_image() -> Option<Vec<u8>> {
    let png_id = fmt_id("PNG");
    if png_id != 0 && IsClipboardFormatAvailable(png_id).is_ok() {
        if let Some(b) = bytes(png_id) {
            if b.len() > 8 && b[1] == b'P' && b[2] == b'N' && b[3] == b'G' {
                return Some(b);
            }
        }
    }
    if IsClipboardFormatAvailable(CF_DIB).is_ok() {
        let dib = bytes(CF_DIB)?;
        return dib_to_png(&dib);
    }
    None
}

pub fn read() -> Option<Clip> {
    if !open(8) {
        return None;
    }
    let res = unsafe { read_inner() };
    unsafe {
        let _ = CloseClipboard();
    }
    res
}

unsafe fn read_inner() -> Option<Clip> {
    if excluded() {
        return None;
    }

    let files = read_files();
    if !files.is_empty() {
        return Some(Clip {
            kind: "files".into(),
            text: files.join("\n"),
            html: None,
            png: None,
            files,
        });
    }

    if let Some(png) = read_image() {
        return Some(Clip {
            kind: "image".into(),
            text: String::new(),
            html: None,
            png: Some(png),
            files: Vec::new(),
        });
    }

    let plain = if IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok() {
        read_text(CF_UNICODETEXT)
    } else {
        None
    };
    let html = read_html();

    let text = match (&plain, &html) {
        (Some(t), _) => t.clone(),
        (None, Some(h)) => strip_tags(h),
        (None, None) => return None,
    };
    if text.trim().is_empty() {
        return None;
    }

    Some(Clip {
        kind: if html.is_some() { "rich".into() } else { "text".into() },
        text,
        html,
        png: None,
        files: Vec::new(),
    })
}

fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

unsafe fn alloc(bytes: &[u8]) -> Option<HGLOBAL> {
    let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).ok()?;
    let p = GlobalLock(h);
    if p.is_null() {
        return None;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
    let _ = GlobalUnlock(h);
    Some(h)
}

unsafe fn put(fmt: u32, data: &[u8]) -> bool {
    if let Some(h) = alloc(data) {
        SetClipboardData(fmt, HANDLE(h.0)).is_ok()
    } else {
        false
    }
}

fn utf16_bytes(s: &str) -> Vec<u8> {
    let mut v: Vec<u8> = Vec::with_capacity(s.len() * 2 + 2);
    for u in s.encode_utf16().chain(std::iter::once(0)) {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v
}

/// Build a valid CF_HTML payload (byte offsets must match exactly).
fn cf_html(fragment: &str) -> Vec<u8> {
    let head_start = "<html><body><!--StartFragment-->";
    let tail = "<!--EndFragment--></body></html>";
    let frag = fragment.trim();
    // Offsets are 10 digits wide, so the header length is stable.
    let header_len = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n".len();
    let start_html = header_len;
    let start_frag = start_html + head_start.len();
    let end_frag = start_frag + frag.len();
    let end_html = end_frag + tail.len();
    let header = format!(
        "Version:0.9\r\nStartHTML:{:010}\r\nEndHTML:{:010}\r\nStartFragment:{:010}\r\nEndFragment:{:010}\r\n",
        start_html, end_html, start_frag, end_frag
    );
    let full = format!("{}{}{}{}", header, head_start, frag, tail);
    full.into_bytes()
}

fn hdrop_bytes(paths: &[String]) -> Vec<u8> {
    // DROPFILES: pFiles(4) pt(8) fNC(4) fWide(4) = 20 bytes
    let mut v = Vec::new();
    v.extend_from_slice(&20u32.to_le_bytes());
    v.extend_from_slice(&0i32.to_le_bytes());
    v.extend_from_slice(&0i32.to_le_bytes());
    v.extend_from_slice(&0i32.to_le_bytes());
    v.extend_from_slice(&1i32.to_le_bytes());
    for p in paths {
        for u in p.encode_utf16() {
            v.extend_from_slice(&u.to_le_bytes());
        }
        v.extend_from_slice(&0u16.to_le_bytes());
    }
    v.extend_from_slice(&0u16.to_le_bytes());
    v
}

pub struct WritePayload<'a> {
    pub text: &'a str,
    pub html: Option<&'a str>,
    pub png: Option<&'a [u8]>,
    pub files: &'a [String],
}

pub fn write(p: &WritePayload) -> bool {
    if !open(10) {
        return false;
    }
    unsafe {
        if EmptyClipboard().is_err() {
            let _ = CloseClipboard();
            return false;
        }
        if !p.text.is_empty() {
            put(CF_UNICODETEXT, &utf16_bytes(p.text));
        }
        if let Some(html) = p.html {
            let id = fmt_id("HTML Format");
            if id != 0 {
                put(id, &cf_html(html));
            }
        }
        if let Some(png) = p.png {
            let id = fmt_id("PNG");
            if id != 0 {
                put(id, png);
            }
            if let Some(dib) = png_to_dib(png) {
                put(CF_DIB, &dib);
            }
        }
        if !p.files.is_empty() {
            put(CF_HDROP, &hdrop_bytes(p.files));
        }
        let _ = CloseClipboard();
    }
    true
}

// ---------------------------------------------------------------- image codec

pub fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }
    let u32at = |o: usize| u32::from_le_bytes([dib[o], dib[o + 1], dib[o + 2], dib[o + 3]]);
    let i32at = |o: usize| i32::from_le_bytes([dib[o], dib[o + 1], dib[o + 2], dib[o + 3]]);
    let header = u32at(0) as usize;
    let w = i32at(4);
    let raw_h = i32at(8);
    let bpp = u16::from_le_bytes([dib[14], dib[15]]);
    let compression = u32at(16);
    if w <= 0 || raw_h == 0 || header < 40 || header > dib.len() {
        return None;
    }
    let top_down = raw_h < 0;
    let w = w as usize;
    let h = raw_h.unsigned_abs() as usize;
    let stride = match bpp {
        32 => w * 4,
        24 => ((w * 3 + 3) / 4) * 4,
        _ => return None,
    };
    if compression != 0 && compression != 3 {
        return None;
    }
    if bpp == 24 && compression != 0 {
        return None;
    }
    let need = header + stride * h;
    if dib.len() < need {
        return None;
    }
    let data = &dib[header..];
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        let src_row = if top_down { y } else { h - 1 - y };
        let row = &data[src_row * stride..src_row * stride + stride];
        for x in 0..w {
            let o = y * w * 4 + x * 4;
            if bpp == 32 {
                let s = x * 4;
                rgba[o] = row[s + 2];
                rgba[o + 1] = row[s + 1];
                rgba[o + 2] = row[s];
                rgba[o + 3] = 255;
            } else {
                let s = x * 3;
                rgba[o] = row[s + 2];
                rgba[o + 1] = row[s + 1];
                rgba[o + 2] = row[s];
                rgba[o + 3] = 255;
            }
        }
    }
    encode_png(w as u32, h as u32, &rgba)
}

fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
    }
    Some(out)
}

pub fn decode_png(png_bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(png_bytes);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    let data = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => {
            let mut v = Vec::with_capacity(w as usize * h as usize * 4);
            for px in buf[..info.buffer_size()].chunks_exact(3) {
                v.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            v
        }
        _ => return None,
    };
    Some((w, h, data))
}

fn png_to_dib(png_bytes: &[u8]) -> Option<Vec<u8>> {
    let (w, h, rgba) = decode_png(png_bytes)?;
    let mut out = Vec::with_capacity(40 + (w as usize * h as usize * 4));
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(w as u32 * h as u32 * 4).to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for y in (0..h).rev() {
        for x in 0..w {
            let o = ((y * w + x) * 4) as usize;
            out.extend_from_slice(&[rgba[o + 2], rgba[o + 1], rgba[o], 255]);
        }
    }
    Some(out)
}

/// Small PNG (max width) as a base64 string, for list previews.
pub fn thumbnail(png_bytes: &[u8], max_w: u32) -> Option<String> {
    let (w, h, rgba) = decode_png(png_bytes)?;
    if w == 0 || h == 0 {
        return None;
    }
    let scale = (max_w as f32 / w as f32).min(1.0);
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    let mut small = vec![0u8; (nw * nh * 4) as usize];
    // box filter with integer accumulation
    let mut acc = [0u32; 4];
    for y in 0..nh {
        for x in 0..nw {
            let x0 = x * w / nw;
            let x1 = (((x + 1) * w + nw - 1) / nw).max(x0 + 1).min(w);
            let y0 = y * h / nh;
            let y1 = (((y + 1) * h + nh - 1) / nh).max(y0 + 1).min(h);
            acc = [0; 4];
            let mut n = 0u32;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let o = ((sy * w + sx) * 4) as usize;
                    acc[0] += rgba[o] as u32;
                    acc[1] += rgba[o + 1] as u32;
                    acc[2] += rgba[o + 2] as u32;
                    acc[3] += rgba[o + 3] as u32;
                    n += 1;
                }
            }
            let o = ((y * nw + x) * 4) as usize;
            n = n.max(1);
            small[o] = (acc[0] / n) as u8;
            small[o + 1] = (acc[1] / n) as u8;
            small[o + 2] = (acc[2] / n) as u8;
            small[o + 3] = (acc[3] / n) as u8;
        }
    }
    let bytes = encode_png(nw, nh, &small)?;
    Some(base64(&bytes))
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity((data.len() + 2) / 3 * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

#[allow(dead_code)]
pub fn unused(_: *const c_void) {}
