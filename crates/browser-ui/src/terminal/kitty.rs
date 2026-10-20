//! Kitty graphics protocol, the subset `kitty +kitten icat`, yazi, chafa and timg use.
//!
//! vte drops APC strings, so `Splitter` pulls `ESC _ G … ESC \` out of the PTY stream before the
//! emulator sees it, and `Graphics` turns each command into a stored image, a placement or a reply.
//! shortcut: no animation, unicode placeholders, z-index, shared memory or relative placements; add when a tool needs them.

use base64::Engine;
use gpui::RenderImage;
use image::RgbaImage;
use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

/// Larger APC payloads are dropped rather than buffered.
const MAX_COMMAND: usize = 64 << 20;
/// Decoded images kept for later `a=p`; the oldest go first past this.
const MAX_STORED: usize = 64 << 20;

pub enum Piece {
    Text(Vec<u8>),
    /// A graphics command without its leading `G`.
    Graphics(Vec<u8>),
}

#[derive(Default, Clone, Copy, PartialEq)]
enum State {
    #[default]
    Text,
    Esc,
    Apc,
    ApcEsc,
}

/// Splits PTY output into text and graphics commands, carrying partial escapes across reads.
#[derive(Default)]
pub struct Splitter {
    state: State,
    apc: Vec<u8>,
}

impl Splitter {
    pub fn split(&mut self, input: &[u8]) -> Vec<Piece> {
        let mut out = Vec::new();
        let mut text = Vec::new();
        let mut rest = input;
        while let Some((&byte, tail)) = rest.split_first() {
            if byte != 0x1b && matches!(self.state, State::Text | State::Apc) {
                // Text and image payloads are most of the stream: copy each run up to the next escape at once.
                let len = rest.iter().position(|&b| b == 0x1b).unwrap_or(rest.len());
                if self.state == State::Text {
                    text.extend_from_slice(&rest[..len]);
                } else {
                    self.push_apc(&rest[..len]);
                }
                rest = &rest[len..];
                continue;
            }
            rest = tail;
            match self.state {
                State::Text if byte == 0x1b => self.state = State::Esc,
                State::Text => text.push(byte),
                State::Esc if byte == b'_' => {
                    self.state = State::Apc;
                    self.apc.clear();
                }
                State::Esc => {
                    text.push(0x1b);
                    if byte != 0x1b {
                        text.push(byte);
                        self.state = State::Text;
                    }
                }
                State::Apc if byte == 0x1b => self.state = State::ApcEsc,
                State::Apc => self.push_apc(&[byte]),
                State::ApcEsc if byte == b'\\' => {
                    self.state = State::Text;
                    if self.apc.first() == Some(&b'G') {
                        if !text.is_empty() {
                            out.push(Piece::Text(std::mem::take(&mut text)));
                        }
                        out.push(Piece::Graphics(self.apc[1..].to_vec()));
                    }
                    self.apc = Vec::new();
                }
                State::ApcEsc => {
                    self.push_apc(&[0x1b, byte]);
                    self.state = State::Apc;
                }
            }
        }
        if !text.is_empty() {
            out.push(Piece::Text(text));
        }
        out
    }

    fn push_apc(&mut self, bytes: &[u8]) {
        if self.apc.len() < MAX_COMMAND {
            self.apc.extend_from_slice(bytes);
        }
    }
}

/// A command's `key=value` pairs, with the protocol's defaults.
#[derive(Clone)]
struct Control {
    action: u8,
    format: u32,
    medium: u8,
    zlib: bool,
    width: u32,
    height: u32,
    id: u32,
    cols: u32,
    rows: u32,
    quiet: u32,
    more: bool,
    delete: u8,
    keep_cursor: bool,
}

impl Control {
    fn parse(text: &str) -> Self {
        let mut c = Self {
            action: b't',
            format: 32,
            medium: b'd',
            zlib: false,
            width: 0,
            height: 0,
            id: 0,
            cols: 0,
            rows: 0,
            quiet: 0,
            more: false,
            delete: b'a',
            keep_cursor: false,
        };
        for pair in text.split(',') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            let number = value.parse::<u32>().unwrap_or(0);
            let letter = value.bytes().next().unwrap_or(0);
            match key {
                "a" => c.action = letter,
                "f" => c.format = number,
                "t" => c.medium = letter,
                "o" => c.zlib = letter == b'z',
                "s" => c.width = number,
                "v" => c.height = number,
                "i" => c.id = number,
                "c" => c.cols = number,
                "r" => c.rows = number,
                "q" => c.quiet = number,
                "m" => c.more = number == 1,
                "d" => c.delete = letter,
                "C" => c.keep_cursor = number == 1,
                _ => {}
            }
        }
        c
    }
}

/// Where and how big to draw an image; the view anchors it at the cursor.
pub struct Show {
    pub image: Arc<RenderImage>,
    /// Image size in pixels.
    pub size: (u32, u32),
    /// Cells asked for with `c`/`r`; 0 means "from the image size".
    pub cols: u32,
    pub rows: u32,
    pub image_id: u32,
    pub move_cursor: bool,
}

pub enum Delete {
    All,
    Id(u32),
}

#[derive(Default)]
pub struct Outcome {
    /// Bytes for the PTY (the protocol's OK/error replies).
    pub reply: Vec<u8>,
    pub show: Option<Show>,
    pub delete: Option<Delete>,
}

struct Stored {
    image: Arc<RenderImage>,
    size: (u32, u32),
    bytes: usize,
    seq: u64,
}

#[derive(Default)]
pub struct Graphics {
    images: HashMap<u32, Stored>,
    /// A chunked (`m=1`) transmission still arriving: its first chunk's control and the payload so far.
    pending: Option<(Control, Vec<u8>)>,
    seq: u64,
    /// Freed images whose GPU atlas tiles the view must release.
    pub retired: Vec<Arc<RenderImage>>,
}

impl Graphics {
    pub fn handle(&mut self, command: &[u8]) -> Outcome {
        let (control, payload) = match command.iter().position(|b| *b == b';') {
            Some(at) => (&command[..at], &command[at + 1..]),
            None => (command, &[][..]),
        };
        let parsed = Control::parse(&String::from_utf8_lossy(control));
        // Later chunks carry only `m` (and `q`); the first chunk's control applies to the whole image.
        let (control, payload) = match self.pending.take() {
            Some((first, mut data)) => {
                // MAX_COMMAND caps each chunk; without this an endless `m=1` stream grows until OOM.
                if data.len() + payload.len() > MAX_COMMAND {
                    return Outcome::default();
                }
                data.extend_from_slice(payload);
                (first, data)
            }
            None => (parsed.clone(), payload.to_vec()),
        };
        if parsed.more {
            self.pending = Some((control, payload));
            return Outcome::default();
        }
        let mut outcome = Outcome::default();
        let result = match control.action {
            b'd' => {
                outcome.delete = Some(self.delete(&control));
                Ok(None)
            }
            b'p' => match self.images.get(&control.id) {
                Some(stored) => Ok(Some((stored.image.clone(), stored.size))),
                None => Err("ENOENT:no such image"),
            },
            b'q' => decode(&control, &payload).map(|_| None),
            b't' | b'T' => decode(&control, &payload).map(|rgba| {
                let size = rgba.dimensions();
                let image = to_render_image(rgba);
                self.store(control.id, image.clone(), size);
                (control.action == b'T').then_some((image, size))
            }),
            _ => Err("EINVAL:unsupported action"),
        };
        match result {
            Ok(shown) => {
                if let Some((image, size)) = shown {
                    outcome.show = Some(Show {
                        image,
                        size,
                        cols: control.cols,
                        rows: control.rows,
                        image_id: control.id,
                        move_cursor: !control.keep_cursor,
                    });
                }
                if control.action != b'd' && control.quiet == 0 {
                    outcome.reply = reply(control.id, "OK");
                }
            }
            Err(error) if control.quiet < 2 => outcome.reply = reply(control.id, error),
            Err(_) => {}
        }
        outcome
    }

    fn store(&mut self, id: u32, image: Arc<RenderImage>, size: (u32, u32)) {
        if id == 0 {
            return;
        }
        self.seq += 1;
        let bytes = size.0 as usize * size.1 as usize * 4;
        if let Some(old) = self.images.insert(id, Stored { image, size, bytes, seq: self.seq }) {
            self.retired.push(old.image);
        }
        while self.images.values().map(|stored| stored.bytes).sum::<usize>() > MAX_STORED && self.images.len() > 1 {
            let Some(oldest) = self.images.iter().min_by_key(|(_, stored)| stored.seq).map(|(id, _)| *id) else {
                break;
            };
            if let Some(old) = self.images.remove(&oldest) {
                self.retired.push(old.image);
            }
        }
    }

    fn delete(&mut self, control: &Control) -> Delete {
        match control.delete {
            b'i' | b'I' => {
                if control.delete == b'I' {
                    if let Some(old) = self.images.remove(&control.id) {
                        self.retired.push(old.image);
                    }
                }
                Delete::Id(control.id)
            }
            // shortcut: every other target (by cell, column, z-index…) clears all placements.
            target => {
                if target.is_ascii_uppercase() {
                    self.retired.extend(self.images.drain().map(|(_, stored)| stored.image));
                }
                Delete::All
            }
        }
    }
}

/// Replies go only to commands that named an image id, as kitty does.
fn reply(id: u32, message: &str) -> Vec<u8> {
    if id == 0 {
        return Vec::new();
    }
    format!("\x1b_Gi={id};{message}\x1b\\").into_bytes()
}

fn decode(control: &Control, payload: &[u8]) -> Result<RgbaImage, &'static str> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let payload: Vec<u8> = payload.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
    let data = b64.decode(&payload).map_err(|_| "EINVAL:bad base64")?;
    let mut data = match control.medium {
        b'd' => data,
        b'f' | b't' => {
            let path = std::path::PathBuf::from(String::from_utf8(data).map_err(|_| "EINVAL:bad path")?);
            // The spec forbids device and kernel files: reading one could block or leak.
            let banned = ["/proc", "/sys", "/dev"].iter().any(|dir| path.starts_with(dir));
            if banned || !path.is_file() {
                return Err("EBADF:not a regular file");
            }
            if std::fs::metadata(&path).map_or(true, |meta| meta.len() > MAX_COMMAND as u64) {
                return Err("EFBIG:file too large");
            }
            let bytes = std::fs::read(&path).map_err(|_| "EBADF:unreadable file")?;
            // Temp files are the terminal's to delete, but only protocol-named ones in a temp directory (per the spec).
            let in_temp = [std::env::temp_dir(), "/tmp".into(), "/private/tmp".into()].iter().any(|dir| path.starts_with(dir));
            let named = path.file_name().is_some_and(|name| name.to_string_lossy().contains("tty-graphics-protocol"));
            if control.medium == b't' && in_temp && named {
                let _ = std::fs::remove_file(&path);
            }
            bytes
        }
        _ => return Err("EINVAL:unsupported transmission medium"),
    };
    if control.zlib {
        let mut inflated = Vec::new();
        flate2::read::ZlibDecoder::new(&data[..])
            .take(MAX_STORED as u64)
            .read_to_end(&mut inflated)
            .map_err(|_| "EINVAL:bad zlib data")?;
        data = inflated;
    }
    match control.format {
        100 => image::load_from_memory_with_format(&data, image::ImageFormat::Png)
            .map(|image| image.into_rgba8())
            .map_err(|_| "EBADPNG:could not decode PNG"),
        24 | 32 => {
            let (w, h) = (control.width, control.height);
            let channels = if control.format == 24 { 3 } else { 4 };
            if w == 0 || h == 0 || data.len() != w as usize * h as usize * channels {
                return Err("EINVAL:size does not match data");
            }
            if channels == 4 {
                return RgbaImage::from_raw(w, h, data).ok_or("EINVAL:bad pixels");
            }
            let rgba = data.as_chunks::<3>().0.iter().flat_map(|px| [px[0], px[1], px[2], 255]).collect();
            RgbaImage::from_raw(w, h, rgba).ok_or("EINVAL:bad pixels")
        }
        _ => Err("EINVAL:unsupported format"),
    }
}

/// GPUI draws BGRA.
fn to_render_image(mut rgba: RgbaImage) -> Arc<RenderImage> {
    for px in rgba.pixels_mut() {
        px.0.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![image::Frame::new(rgba)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(RgbaImage::new(w, h)).write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn graphics(pieces: Vec<Piece>) -> Vec<Vec<u8>> {
        pieces.into_iter().filter_map(|piece| if let Piece::Graphics(g) = piece { Some(g) } else { None }).collect()
    }

    #[test]
    fn text_passes_through_and_split_commands_are_rejoined() {
        let mut splitter = Splitter::default();
        let first = splitter.split(b"hi \x1b[1mbold\x1b_Ga=q,i=1;AAA");
        assert!(matches!(&first[..], [Piece::Text(t)] if t == b"hi \x1b[1mbold"));
        let second = splitter.split(b"A\x1b");
        assert!(second.is_empty());
        let third = splitter.split(b"\\after\x1b_Xother\x1b\\");
        assert!(matches!(&third[..], [Piece::Graphics(g), Piece::Text(t)] if g == b"a=q,i=1;AAAA" && t == b"after"));
    }

    #[test]
    fn lone_escape_at_a_read_boundary_is_kept() {
        let mut splitter = Splitter::default();
        assert!(splitter.split(b"\x1b").is_empty());
        assert!(matches!(&splitter.split(b"[A")[..], [Piece::Text(t)] if t == b"\x1b[A"));
    }

    #[test]
    fn query_gets_ok_so_icat_uses_graphics() {
        let mut g = Graphics::default();
        let pixel = b64(&[1, 2, 3]);
        let outcome = g.handle(format!("a=q,i=31,s=1,v=1,f=24,t=d;{pixel}").as_bytes());
        assert_eq!(outcome.reply, b"\x1b_Gi=31;OK\x1b\\");
        assert!(outcome.show.is_none());
        assert!(g.handle(b"a=q,i=32,s=1,v=1,f=24,t=d;AAAAAAAA").reply.ends_with(b"EINVAL:size does not match data\x1b\\"));
    }

    #[test]
    fn chunked_png_is_reassembled_stored_and_placed_again() {
        let data = b64(&png(3, 2));
        let (head, tail) = data.split_at(8);
        let mut g = Graphics::default();
        let mut splitter = Splitter::default();
        let stream = format!("\x1b_Ga=T,f=100,i=7,q=2,m=1;{head}\x1b\\\x1b_Gm=0;{tail}\x1b\\");
        let mut outcomes = graphics(splitter.split(stream.as_bytes())).into_iter().map(|cmd| g.handle(&cmd)).collect::<Vec<_>>();
        let last = outcomes.pop().unwrap();
        assert!(outcomes[0].show.is_none());
        let show = last.show.expect("shown");
        assert_eq!(show.size, (3, 2));
        assert!(show.move_cursor && last.reply.is_empty());
        assert!(g.handle(b"a=p,i=7").show.is_some());
        assert!(matches!(g.handle(b"a=d,d=I,i=7").delete, Some(Delete::Id(7))));
        assert_eq!(g.retired.len(), 1);
        assert!(g.handle(b"a=p,i=7").reply.ends_with(b"ENOENT:no such image\x1b\\"));
    }

    #[test]
    fn temp_files_are_read_and_removed() {
        let path = std::env::temp_dir().join(format!("tty-graphics-protocol-test-{}.png", std::process::id()));
        std::fs::write(&path, png(1, 1)).unwrap();
        let mut g = Graphics::default();
        let outcome = g.handle(format!("a=T,f=100,t=t;{}", b64(path.to_string_lossy().as_bytes())).as_bytes());
        assert!(outcome.show.is_some());
        assert!(!path.exists());
        assert!(g.handle(format!("a=T,f=100,t=f,i=2;{}", b64(b"/dev/zero")).as_bytes()).reply.ends_with(b"EBADF:not a regular file\x1b\\"));
    }

    #[test]
    fn temp_medium_never_deletes_outside_temp_dirs() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tty-graphics-protocol-keep-{}.png", std::process::id()));
        std::fs::write(&path, png(1, 1)).unwrap();
        let outcome = Graphics::default().handle(format!("a=T,f=100,t=t;{}", b64(path.to_string_lossy().as_bytes())).as_bytes());
        let kept = path.exists();
        let _ = std::fs::remove_file(&path);
        assert!(outcome.show.is_some() && kept);
    }
}
