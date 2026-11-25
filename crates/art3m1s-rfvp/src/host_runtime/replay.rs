use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, SyncSender};
use std::thread::{self, JoinHandle};

use rfvp::host_api::{RenderCommand, RenderFrame, TextureFormat, TextureHandle};
use rfvp::rendering::external::{
    RecordedTextureCommand, RecordedTextureCreate, RecordedTextureUpdate,
};

use super::{
    NativeFrame, NativeSession, RfvpAssetSource, RfvpHostInputEvent, RfvpHostRuntimeError, RfvpNls,
    RfvpSaveStore,
};

/// A complete RFVP frame. Upload `textures` before drawing `frame.commands`.
/// Each referenced handle has one texture, in order of first use. Texture
/// generations are scoped to the session. Unchanged textures are shared.
#[derive(Debug)]
pub struct RfvpReplayFrame {
    pub width: u32,
    pub height: u32,
    pub frame: RenderFrame,
    pub textures: Vec<Arc<RecordedTextureCreate>>,
}

#[derive(Debug)]
pub enum RfvpReplayError {
    Runtime(RfvpHostRuntimeError),
    AssetRoot(super::RfvpError),
    MissingTexture(TextureHandle),
    InvalidTexture(TextureHandle),
    FrameStreamStopped,
}

impl fmt::Display for RfvpReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => error.fmt(f),
            Self::AssetRoot(error) => write!(f, "RFVP asset root: {error}"),
            Self::MissingTexture(handle) => write!(f, "RFVP texture {} is missing", handle.0),
            Self::InvalidTexture(handle) => {
                write!(f, "invalid RFVP texture data for handle {}", handle.0)
            }
            Self::FrameStreamStopped => f.write_str("RFVP frame stream stopped after an error"),
        }
    }
}

impl std::error::Error for RfvpReplayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Runtime(error) => Some(error),
            _ => None,
        }
    }
}

impl From<RfvpHostRuntimeError> for RfvpReplayError {
    fn from(error: RfvpHostRuntimeError) -> Self {
        Self::Runtime(error)
    }
}

/// Drives an RFVP game without a GPU backend.
///
/// Keep session calls on the creating thread. RFVP's script globals are
/// process-wide, so run one game at a time across the RFVP entry points.
pub struct RfvpFrameRuntime {
    pub(super) session: NativeSession,
    stage_size: (u32, u32),
    textures: FrameTextures,
    latest: Option<Arc<RfvpReplayFrame>>,
    stopped: bool,
}

impl RfvpFrameRuntime {
    pub fn new_directory(
        game_root: impl AsRef<Path>,
        save_root: Option<&Path>,
        nls: RfvpNls,
    ) -> Result<Self, RfvpReplayError> {
        use super::directory::{DirectoryAssets, DirectorySaves};
        let project = game_root.as_ref();
        let assets = DirectoryAssets::new(project).map_err(RfvpReplayError::AssetRoot)?;
        let saves = save_root
            .map(Path::to_owned)
            .unwrap_or_else(|| project.join("save"));
        Self::open_with_io(Arc::new(assets), Arc::new(DirectorySaves(saves)), nls)
    }

    /// Opens a project whose assets and persistent saves are supplied by the Host.
    pub fn open_with_io(
        assets: Arc<dyn RfvpAssetSource>,
        saves: Arc<dyn RfvpSaveStore>,
        nls: RfvpNls,
    ) -> Result<Self, RfvpReplayError> {
        let (session, stage_size) = NativeSession::open_replay(assets, saves, nls)?;
        Ok(Self {
            session,
            stage_size,
            textures: FrameTextures::default(),
            latest: None,
            stopped: false,
        })
    }

    pub fn push_input(&mut self, events: &[RfvpHostInputEvent]) -> Result<(), RfvpReplayError> {
        if self.stopped {
            return Err(RfvpReplayError::FrameStreamStopped);
        }
        Ok(self.session.push_input(events)?)
    }

    pub fn is_exit_requested(&self) -> bool {
        self.session.is_exit_requested()
    }

    /// Consumes the native frame on every step, even when the Host skips presentation.
    /// A capture error stops the stream because a consumed texture delta cannot be retried.
    pub fn step(&mut self, delta_ms: u32) -> Result<(), RfvpReplayError> {
        if self.stopped {
            return Err(RfvpReplayError::FrameStreamStopped);
        }
        self.latest = None;
        match self.capture(delta_ms) {
            Ok(frame) => {
                self.latest = frame.map(Arc::new);
                Ok(())
            }
            Err(error) => {
                self.stopped = true;
                Err(error)
            }
        }
    }

    /// Shares the newest complete frame. It remains valid on other threads
    /// and after later steps or session destruction, until the last owner drops it.
    pub fn acquire_frame(&self) -> Option<Arc<RfvpReplayFrame>> {
        self.latest.clone()
    }

    fn capture(&mut self, delta_ms: u32) -> Result<Option<RfvpReplayFrame>, RfvpReplayError> {
        self.session.step(delta_ms)?;
        self.session
            .take_frame()?
            .map(|native| self.textures.snapshot(native))
            .transpose()
    }
}

thread_local! {
    static REPLAY_WORKER: Cell<bool> = const { Cell::new(false) };
}

pub fn is_replay_worker_thread() -> bool {
    REPLAY_WORKER.get()
}

type Job = Box<dyn FnOnce(&mut RfvpFrameRuntime) + Send>;

pub(crate) struct ReplayWorker {
    jobs: Option<SyncSender<Job>>,
    thread: Option<JoinHandle<()>>,
    pub(crate) stage_size: (u32, u32),
}

impl ReplayWorker {
    pub(crate) fn open(
        project: PathBuf,
        saves: Option<PathBuf>,
        nls: RfvpNls,
    ) -> Result<Self, RfvpReplayError> {
        Self::start(move || RfvpFrameRuntime::new_directory(project, saves.as_deref(), nls))
    }

    pub(crate) fn open_with_io(
        assets: Arc<dyn RfvpAssetSource>,
        saves: Arc<dyn RfvpSaveStore>,
        nls: RfvpNls,
    ) -> Result<Self, RfvpReplayError> {
        Self::start(move || RfvpFrameRuntime::open_with_io(assets, saves, nls))
    }

    fn start(
        open: impl FnOnce() -> Result<RfvpFrameRuntime, RfvpReplayError> + Send + 'static,
    ) -> Result<Self, RfvpReplayError> {
        let (jobs, queue) = mpsc::sync_channel::<Job>(0);
        let (ready, started) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("rfvp-session".into())
            .spawn(move || {
                REPLAY_WORKER.set(true);
                match open() {
                    Ok(mut runtime) => {
                        if ready.send(Ok(runtime.stage_size)).is_ok() {
                            for job in queue {
                                job(&mut runtime);
                            }
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                }
            })
            .map_err(|_| RfvpHostRuntimeError::WorkerStopped)?;
        let mut worker = Self {
            jobs: Some(jobs),
            thread: Some(thread),
            stage_size: (0, 0),
        };
        worker.stage_size = started
            .recv()
            .map_err(|_| RfvpHostRuntimeError::WorkerStopped)??;
        Ok(worker)
    }

    pub(crate) fn run<R: Send + 'static>(
        &self,
        job: impl FnOnce(&mut RfvpFrameRuntime) -> R + Send + 'static,
    ) -> Result<R, RfvpHostRuntimeError> {
        let (reply, result) = mpsc::sync_channel(1);
        self.jobs
            .as_ref()
            .ok_or(RfvpHostRuntimeError::WorkerStopped)?
            .send(Box::new(move |runtime| {
                let _ = reply.send(job(runtime));
            }))
            .map_err(|_| RfvpHostRuntimeError::WorkerStopped)?;
        result
            .recv()
            .map_err(|_| RfvpHostRuntimeError::WorkerStopped)
    }
}

impl Drop for ReplayWorker {
    fn drop(&mut self) {
        drop(self.jobs.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
struct FrameTextures {
    live: HashMap<TextureHandle, Arc<RecordedTextureCreate>>,
    generation: u64,
}

impl FrameTextures {
    fn snapshot(&mut self, native: NativeFrame) -> Result<RfvpReplayFrame, RfvpReplayError> {
        self.generation += 1;
        for command in native.texture_commands {
            match command {
                RecordedTextureCommand::Create(mut texture) => {
                    let stride = pixel_stride(texture.handle, texture.desc.format)?;
                    if byte_len(
                        texture.desc.width.into(),
                        texture.desc.height.into(),
                        stride,
                    ) != Some(texture.pixels.len())
                    {
                        return Err(RfvpReplayError::InvalidTexture(texture.handle));
                    }
                    texture.generation = self.generation;
                    self.live.insert(texture.handle, Arc::new(texture));
                }
                RecordedTextureCommand::Update(update) => self.update(update)?,
                RecordedTextureCommand::Destroy(texture) => {
                    self.live.remove(&texture.handle);
                }
            }
        }
        let mut textures = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for command in &native.frame.commands {
            let handle = match command {
                RenderCommand::DrawImage(command) => command.texture,
                RenderCommand::DrawGlyph(command) => command.texture,
                RenderCommand::SetClip(_) | RenderCommand::ClearClip => continue,
            };
            if seen.insert(handle) {
                let texture = self
                    .live
                    .get(&handle)
                    .ok_or(RfvpReplayError::MissingTexture(handle))?;
                textures.push(Arc::clone(texture));
            }
        }
        Ok(RfvpReplayFrame {
            width: native.width,
            height: native.height,
            frame: native.frame,
            textures,
        })
    }

    fn update(&mut self, update: RecordedTextureUpdate) -> Result<(), RfvpReplayError> {
        let handle = update.handle;
        let texture = self
            .live
            .get_mut(&handle)
            .ok_or(RfvpReplayError::MissingTexture(handle))?;
        let rect = update.rect;
        let stride = pixel_stride(handle, update.format)?;
        if update.format != texture.desc.format
            || rect
                .x
                .checked_add(rect.width)
                .is_none_or(|end| end > texture.desc.width.into())
            || rect
                .y
                .checked_add(rect.height)
                .is_none_or(|end| end > texture.desc.height.into())
            || byte_len(rect.width, rect.height, stride) != Some(update.pixels.len())
        {
            return Err(RfvpReplayError::InvalidTexture(handle));
        }
        if update.pixels.is_empty() {
            return Ok(());
        }
        let texture = Arc::make_mut(texture);
        let row_stride = usize::from(texture.desc.width) * stride;
        let start = rect.y as usize * row_stride + rect.x as usize * stride;
        let row_bytes = rect.width as usize * stride;
        for (row, source) in update.pixels.chunks_exact(row_bytes).enumerate() {
            let offset = start + row * row_stride;
            texture.pixels[offset..offset + row_bytes].copy_from_slice(source);
        }
        texture.generation = self.generation;
        Ok(())
    }
}

fn pixel_stride(handle: TextureHandle, format: TextureFormat) -> Result<usize, RfvpReplayError> {
    match format {
        TextureFormat::Rgba8 => Ok(4),
        TextureFormat::LumaA8 => Ok(2),
        _ => Err(RfvpReplayError::InvalidTexture(handle)),
    }
}

fn byte_len(width: u32, height: u32, stride: usize) -> Option<usize> {
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(stride)
}

#[cfg(test)]
mod tests;
