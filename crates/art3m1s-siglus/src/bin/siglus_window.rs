#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use art3m1s_render::backend::metal::MetalBackend;
use art3m1s_render::{Extent2D, GpuBackend, NativeSurface, NativeSurfaceKind};
use art3m1s_siglus::{SiglusAdapter, is_siglus_project};
use objc2_app_kit::NSView;
use objc2_quartz_core::CAMetalLayer;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use siglus_scene_vm::host::SiglusHostConfig;
use siglus_scene_vm::runtime::input::{VmKey, VmMouseButton};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::run_on_demand::EventLoopExtRunOnDemand;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowAttributes, WindowId};

struct App {
    game_root: PathBuf,
    window: Option<Box<dyn Window>>,
    layer: Option<objc2::rc::Retained<CAMetalLayer>>,
    gpu: Option<MetalBackend>,
    adapter: Option<SiglusAdapter>,
    last_step: Instant,
    next_redraw: Instant,
    error: Option<anyhow::Error>,
}

impl App {
    fn new(game_root: PathBuf) -> Self {
        Self {
            game_root,
            window: None,
            layer: None,
            gpu: None,
            adapter: None,
            last_step: Instant::now(),
            next_redraw: Instant::now(),
            error: None,
        }
    }

    fn create(&mut self, event_loop: &dyn ActiveEventLoop) -> Result<()> {
        let adapter = SiglusAdapter::open(SiglusHostConfig::new(self.game_root.clone()))?;
        let (width, height) = adapter.logical_size();
        let window = event_loop.create_window(
            WindowAttributes::default()
                .with_title("Art3m1s Siglus renderer")
                .with_surface_size(LogicalSize::new(width as f64, height as f64))
                .with_resizable(false),
        )?;
        let handle = window.window_handle()?.as_raw();
        let view_ptr = match handle {
            RawWindowHandle::AppKit(handle) => handle.ns_view.as_ptr(),
            _ => return Err(anyhow!("winit did not provide an AppKit NSView")),
        };
        // The NSView is owned by the live winit Window for the full App lifetime.
        let view = unsafe { &*view_ptr.cast::<NSView>() };
        let layer = CAMetalLayer::new();
        view.setWantsLayer(true);
        view.setLayer(Some(&layer));

        let mut gpu = MetalBackend::new(width, height).map_err(anyhow::Error::msg)?;
        gpu.set_native_surface(NativeSurface {
            kind: NativeSurfaceKind::AppleMetalLayer,
            handle: (&*layer as *const CAMetalLayer).cast_mut().cast(),
            extent: Extent2D::new(width, height),
        })
        .map_err(anyhow::Error::msg)?;
        eprintln!("Siglus window ready: {width}x{height}; close the window to exit");
        window.request_redraw();
        self.window = Some(window);
        self.layer = Some(layer);
        self.gpu = Some(gpu);
        self.adapter = Some(adapter);
        self.last_step = Instant::now();
        self.next_redraw = self.last_step + Duration::from_millis(16);
        Ok(())
    }

    fn fail(&mut self, event_loop: &dyn ActiveEventLoop, error: anyhow::Error) {
        eprintln!("Siglus window error: {error:#}");
        self.error = Some(error);
        event_loop.exit();
    }

    fn pointer_position(position: winit::dpi::PhysicalPosition<f64>, scale: f64) -> (f64, f64) {
        let point = position.to_logical::<f64>(scale);
        (point.x, point.y)
    }
}

impl ApplicationHandler for App {
    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        if self.window.is_none() {
            if let Err(error) = self.create(event_loop) {
                self.fail(event_loop, error);
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &dyn ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.window.as_ref().is_none_or(|w| w.id() != window_id) {
            return;
        }
        let scale = self.window.as_ref().map_or(1.0, |w| w.scale_factor());
        let Some(adapter) = self.adapter.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                let dt = self.last_step.elapsed().as_millis().min(100) as u32;
                self.last_step = Instant::now();
                let Some(gpu) = self.gpu.as_mut() else { return };
                match adapter.step(dt.max(1), gpu) {
                    Ok(true) => event_loop.exit(),
                    Ok(false) => {}
                    Err(error) => self.fail(event_loop, error),
                }
            }
            WindowEvent::PointerMoved {
                position,
                primary: true,
                ..
            }
            | WindowEvent::PointerEntered {
                position,
                primary: true,
                ..
            } => {
                let point = Self::pointer_position(position, scale);
                adapter.host().mouse_move(point.0, point.1);
            }
            WindowEvent::PointerButton {
                state,
                button,
                position,
                primary: true,
                ..
            } => {
                let point = Self::pointer_position(position, scale);
                let host = adapter.host();
                host.mouse_move(point.0, point.1);
                if let Some(button) = button.mouse_button().and_then(map_mouse_button) {
                    match (button, state) {
                        (VmMouseButton::Left, ElementState::Pressed) => {
                            host.touch(0, point.0, point.1)
                        }
                        (VmMouseButton::Left, ElementState::Released) => {
                            host.touch(2, point.0, point.1)
                        }
                        (button, ElementState::Pressed) => host.mouse_down(button),
                        (button, ElementState::Released) => host.mouse_up(button),
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y * 120.0) as i32,
                    MouseScrollDelta::PixelDelta(p) => p.y.round() as i32,
                    _ => 0,
                };
                adapter.host().mouse_wheel(dy);
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state,
                        physical_key: PhysicalKey::Code(code),
                        text,
                        ..
                    },
                ..
            } => {
                if let Some(key) = map_keycode(code) {
                    match state {
                        ElementState::Pressed => adapter.host().key_down(key),
                        ElementState::Released => adapter.host().key_up(key),
                    }
                }
                if state == ElementState::Pressed {
                    if let Some(text) = text.as_deref() {
                        if adapter.host().vm_mut().ctx.editbox_accepts_direct_text() {
                            adapter.host().text_input(text);
                        }
                    }
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) => adapter.host().text_input(&text),
            WindowEvent::Ime(Ime::Preedit(text, cursor)) => {
                adapter.host().ime_preedit(&text, cursor)
            }
            WindowEvent::Ime(Ime::Disabled) => adapter.host().ime_disabled(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &dyn ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_redraw {
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
            self.next_redraw = now + Duration::from_millis(16);
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_redraw));
    }
}

fn map_mouse_button(button: MouseButton) -> Option<VmMouseButton> {
    match button {
        MouseButton::Left => Some(VmMouseButton::Left),
        MouseButton::Right => Some(VmMouseButton::Right),
        MouseButton::Middle => Some(VmMouseButton::Middle),
        _ => None,
    }
}

fn map_keycode(code: KeyCode) -> Option<VmKey> {
    use KeyCode::*;
    Some(match code {
        Escape => VmKey::Escape,
        Enter | NumpadEnter => VmKey::Enter,
        Space => VmKey::Space,
        Backspace => VmKey::Backspace,
        Delete => VmKey::Delete,
        Tab => VmKey::Tab,
        ArrowLeft => VmKey::ArrowLeft,
        ArrowUp => VmKey::ArrowUp,
        ArrowRight => VmKey::ArrowRight,
        ArrowDown => VmKey::ArrowDown,
        KeyA => VmKey::Letter('A'),
        KeyB => VmKey::Letter('B'),
        KeyC => VmKey::Letter('C'),
        KeyD => VmKey::Letter('D'),
        KeyE => VmKey::Letter('E'),
        KeyF => VmKey::Letter('F'),
        KeyG => VmKey::Letter('G'),
        KeyH => VmKey::Letter('H'),
        KeyI => VmKey::Letter('I'),
        KeyJ => VmKey::Letter('J'),
        KeyK => VmKey::Letter('K'),
        KeyL => VmKey::Letter('L'),
        KeyM => VmKey::Letter('M'),
        KeyN => VmKey::Letter('N'),
        KeyO => VmKey::Letter('O'),
        KeyP => VmKey::Letter('P'),
        KeyQ => VmKey::Letter('Q'),
        KeyR => VmKey::Letter('R'),
        KeyS => VmKey::Letter('S'),
        KeyT => VmKey::Letter('T'),
        KeyU => VmKey::Letter('U'),
        KeyV => VmKey::Letter('V'),
        KeyW => VmKey::Letter('W'),
        KeyX => VmKey::Letter('X'),
        KeyY => VmKey::Letter('Y'),
        KeyZ => VmKey::Letter('Z'),
        Digit0 => VmKey::Digit(0),
        Digit1 => VmKey::Digit(1),
        Digit2 => VmKey::Digit(2),
        Digit3 => VmKey::Digit(3),
        Digit4 => VmKey::Digit(4),
        Digit5 => VmKey::Digit(5),
        Digit6 => VmKey::Digit(6),
        Digit7 => VmKey::Digit(7),
        Digit8 => VmKey::Digit(8),
        Digit9 => VmKey::Digit(9),
        F1 => VmKey::F(1),
        F2 => VmKey::F(2),
        F3 => VmKey::F(3),
        F4 => VmKey::F(4),
        F5 => VmKey::F(5),
        F6 => VmKey::F(6),
        F7 => VmKey::F(7),
        F8 => VmKey::F(8),
        F9 => VmKey::F(9),
        F10 => VmKey::F(10),
        F11 => VmKey::F(11),
        F12 => VmKey::F(12),
        _ => return None,
    })
}

fn main() -> Result<()> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .context("usage: siglus_window GAME_DIR")?;
    if !is_siglus_project(&root) {
        return Err(anyhow!(
            "{} is not a recognized Siglus project",
            root.display()
        ));
    }
    let mut event_loop = EventLoop::new()?;
    let mut app = App::new(root);
    event_loop.run_app_on_demand(&mut app)?;
    if let Some(error) = app.error {
        return Err(error);
    }
    Ok(())
}
