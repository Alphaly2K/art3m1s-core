use std::fmt;
use std::io::{self, Read, Write};

const MAGIC: [u8; 4] = *b"ADX1";
const COMPILE: u32 = 1;
const DRAW: u32 = 2;
const RELEASE: u32 = 3;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Argument(&'static str),
    Protocol(&'static str),
    Worker { status: i32, diagnostic: String },
}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Argument(message) | Self::Protocol(message) => f.write_str(message),
            Self::Worker { status, diagnostic } => {
                write!(f, "D3D11 worker {status:#010x}: {diagnostic}")
            }
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Program(u32);

#[derive(Clone, Copy)]
pub struct Image<'a> {
    pub width: u32,
    pub height: u32,
    pub bgra: &'a [u8],
}

pub struct Draw<'a> {
    pub width: u32,
    pub height: u32,
    pub alpha_blend: bool,
    pub point_sampler: bool,
    pub clear_rgba: [f32; 4],
    pub quad: [[f32; 6]; 4],
    pub uniforms: &'a [(&'a str, &'a [f32])],
    pub textures: [Option<Image<'a>>; 4],
}

/// Caller supplies one worker's stdout and stdin. I/O is synchronous; run it
/// off the presentation thread and let the Host supervise the process.
pub struct Worker<R, W> {
    reader: R,
    writer: W,
    next_id: u32,
    usable: bool,
}

impl<R: Read, W: Write> Worker<R, W> {
    pub fn connect(reader: R, writer: W) -> Result<Self, Error> {
        let mut worker = Self {
            reader,
            writer,
            next_id: 0,
            usable: true,
        };
        worker.reply(0, 0, 0)?;
        Ok(worker)
    }

    pub fn compile(&mut self, source: &[u8]) -> Result<Program, Error> {
        if source.is_empty() || source.len() > MAX_SOURCE_BYTES {
            return Err(Error::Argument("invalid shader source size"));
        }
        let (id, _) = self.exchange(COMPILE, source.len(), 0, |writer| writer.write_all(source))?;
        Ok(Program(id))
    }

    pub fn draw(&mut self, program: Program, draw: &Draw<'_>) -> Result<Vec<u8>, Error> {
        let result = image_len(draw.width, draw.height)?;
        let prefix = draw.encode_prefix(program)?;
        let mut bytes = prefix.len() + 8 * draw.textures.len();
        for image in draw.textures.iter().flatten() {
            if image.bgra.len() != image_len(image.width, image.height)? {
                return Err(Error::Argument("invalid shader texture byte length"));
            }
            bytes += image.bgra.len();
        }
        if bytes > MAX_BYTES {
            return Err(Error::Argument("shader request exceeds image budget"));
        }
        self.exchange(DRAW, bytes, result, |writer| {
            writer.write_all(&prefix)?;
            for image in draw.textures {
                if let Some(image) = image {
                    let mut dimensions = [0; 8];
                    dimensions[..4].copy_from_slice(&image.width.to_le_bytes());
                    dimensions[4..].copy_from_slice(&image.height.to_le_bytes());
                    writer.write_all(&dimensions)?;
                    writer.write_all(image.bgra)?;
                } else {
                    writer.write_all(&[0; 8])?;
                }
            }
            Ok(())
        })
        .map(|(_, pixels)| pixels)
    }

    pub fn release(&mut self, program: Program) -> Result<(), Error> {
        self.exchange(RELEASE, 4, 0, |writer| {
            writer.write_all(&program.0.to_le_bytes())
        })
        .map(|_| ())
    }

    fn exchange(
        &mut self,
        op: u32,
        bytes: usize,
        expected: usize,
        write_body: impl FnOnce(&mut W) -> io::Result<()>,
    ) -> Result<(u32, Vec<u8>), Error> {
        if !self.usable {
            return Err(Error::Protocol("worker connection is closed"));
        }
        let id = self
            .next_id
            .checked_add(1)
            .ok_or(Error::Protocol("worker request IDs exhausted"))?;
        self.next_id = id;
        let tx = (|| {
            let mut header = [0; 16];
            header[..4].copy_from_slice(&MAGIC);
            header[4..8].copy_from_slice(&op.to_le_bytes());
            header[8..12].copy_from_slice(&id.to_le_bytes());
            header[12..].copy_from_slice(&(bytes as u32).to_le_bytes());
            self.writer.write_all(&header)?;
            write_body(&mut self.writer)?;
            self.writer.flush()?;
            self.reply(op, id, expected).map(|data| (id, data))
        })();
        if matches!(tx, Err(Error::Io(_) | Error::Protocol(_))) {
            self.usable = false;
        }
        tx
    }

    fn reply(&mut self, op: u32, id: u32, expected: usize) -> Result<Vec<u8>, Error> {
        let mut header = [0; 16];
        self.reader.read_exact(&mut header)?;
        if header[..4] != MAGIC || word(&header, 4) != op || word(&header, 8) != id {
            return Err(Error::Protocol("worker reply identity does not match"));
        }
        let bytes = (word(&header, 12) as usize)
            .checked_sub(4)
            .filter(|&size| size <= MAX_BYTES)
            .ok_or(Error::Protocol("invalid worker reply size"))?;
        let mut status = [0; 4];
        self.reader.read_exact(&mut status)?;
        let status = i32::from_le_bytes(status);
        if (status == 0 && bytes != expected) || (status < 0 && bytes > 65536) || status > 0 {
            return Err(Error::Protocol("unexpected worker reply payload"));
        }
        let mut data = vec![0; bytes];
        self.reader.read_exact(&mut data)?;
        if status < 0 {
            let diagnostic = String::from_utf8_lossy(&data)
                .trim_end_matches('\0')
                .to_owned();
            return Err(Error::Worker { status, diagnostic });
        }
        Ok(data)
    }
}

fn word(header: &[u8; 16], offset: usize) -> u32 {
    u32::from_le_bytes(header[offset..offset + 4].try_into().unwrap())
}

fn image_len(width: u32, height: u32) -> Result<usize, Error> {
    let bytes = u64::from(width) * u64::from(height);
    if width == 0
        || height == 0
        || width > 16384
        || height > 16384
        || bytes > (MAX_BYTES / 4) as u64
    {
        return Err(Error::Argument("invalid shader image extent"));
    }
    Ok(bytes as usize * 4)
}

impl Draw<'_> {
    fn encode_prefix(&self, program: Program) -> Result<Vec<u8>, Error> {
        if self.uniforms.len() > 256 {
            return Err(Error::Argument("too many shader uniforms"));
        }
        let mut body = Vec::with_capacity(16 + 28 * 4 + 4);
        let flags = u32::from(self.alpha_blend) | (u32::from(self.point_sampler) << 1);
        for value in [program.0, self.width, self.height, flags] {
            body.extend_from_slice(&value.to_le_bytes());
        }
        for value in self.clear_rgba.iter().chain(self.quad.iter().flatten()) {
            body.extend_from_slice(&value.to_le_bytes());
        }
        body.extend_from_slice(&(self.uniforms.len() as u32).to_le_bytes());
        for &(name, values) in self.uniforms {
            if name.is_empty()
                || name.len() > 256
                || name.contains('\0')
                || !(1..=4).contains(&values.len())
            {
                return Err(Error::Argument("invalid shader uniform name or shape"));
            }
            body.extend_from_slice(&(name.len() as u32).to_le_bytes());
            body.extend_from_slice(name.as_bytes());
            body.extend_from_slice(&(values.len() as u32).to_le_bytes());
            for value in values {
                body.extend_from_slice(&value.to_le_bytes());
            }
        }
        Ok(body)
    }
}
