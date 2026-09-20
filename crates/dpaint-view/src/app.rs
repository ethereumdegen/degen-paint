//! The window: `winit` events in, frames out.
//!
//! Everything with a right answer lives in the pure modules; this one owns the surface,
//! translates platform events into [`crate::input`] intent, and keeps the clock. It is
//! the only file in the crate that `winit` appears in.

use dpaint_core::{DocId, Result, Workspace};
use dpaint_gpu::{Gpu, Lighting};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::camera::OrbitCamera;
use crate::canvas::CanvasView;
use crate::input::{self, Action, Button, Drag, Mode, Mods, Wheel};
use crate::reload::{self, JournalWatch};
use crate::render::ViewRenderer;
use crate::status::{self, Status};
use crate::subject::{self, Subject};
use crate::viewport::Viewport;

/// Frame time is smoothed before it reaches the status bar, because an unsmoothed
/// millisecond counter is unreadable and hides the trend that matters.
const FRAME_SMOOTHING: f32 = 0.9;

/// How often the status readout is re-rasterized. Eight times a second is legible and
/// costs nothing; per-frame would put a CPU text rasterization in the drag loop.
const STATUS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(125);

pub struct ViewApp {
    gpu: Gpu,
    doc: DocId,
    ws: Workspace,
    subject: Subject,
    watch: JournalWatch,

    orbit: OrbitCamera,
    canvas: CanvasView,
    viewport: Viewport,
    samples: u32,

    window: Option<Arc<Window>>,
    surface: Option<wgpu::Surface<'static>>,
    config: Option<wgpu::SurfaceConfiguration>,
    renderer: Option<ViewRenderer>,

    cursor: [f32; 2],
    drag: Option<(Button, Drag)>,
    mods: Mods,

    last_frame: Option<Instant>,
    frame_ms: f32,
    show_status: bool,
    status_due: Instant,
    fatal: Option<String>,
}

impl ViewApp {
    /// Open a project and prepare everything a window needs, before the window exists:
    /// a project that will not load should fail on the command line, not as a blank
    /// window that closes itself.
    pub fn new(gpu: Gpu, root: PathBuf, doc_hint: Option<&str>, samples: u32) -> Result<Self> {
        let mut ws = Workspace::open(&root)?;
        let doc = ws.project.resolve_doc(doc_hint)?;
        let seq = reload::newest_seq(&mut ws)?;
        let orbit = OrbitCamera::default();
        let subject = subject::load(
            &ws,
            &doc,
            orbit.camera(),
            Lighting::default(),
            gpu.device().limits().max_texture_dimension_2d,
        )?;
        let watch = JournalWatch::new(&root, seq);
        Ok(Self {
            gpu,
            doc,
            ws,
            subject,
            watch,
            orbit,
            canvas: CanvasView::default(),
            viewport: Viewport::default(),
            samples,
            window: None,
            surface: None,
            config: None,
            renderer: None,
            cursor: [0.0, 0.0],
            drag: None,
            mods: Mods::default(),
            last_frame: None,
            frame_ms: 0.0,
            show_status: true,
            status_due: Instant::now(),
            fatal: None,
        })
    }

    pub fn mode(&self) -> Mode {
        self.subject.mode()
    }

    fn subject_kind(&self) -> String {
        self.ws
            .project
            .doc(&self.doc)
            .map(|d| d.kind().to_string())
            .unwrap_or_else(|_| "?".to_string())
    }

    /// An error the event loop could not recover from, reported after `run` returns so
    /// the process can exit non-zero instead of pretending it drew something.
    pub fn fatal(&self) -> Option<&str> {
        self.fatal.as_deref()
    }

    pub fn title(&self) -> String {
        let name = self
            .ws
            .project
            .doc(&self.doc)
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| self.doc.to_string());
        format!("{} — {} — degen-paint", name, self.ws.project.name)
    }

    fn status_lines(&self) -> Vec<String> {
        let (kind, name) = match self.ws.project.doc(&self.doc) {
            Ok(d) => (d.kind().to_string(), d.name().to_string()),
            Err(_) => ("?".to_string(), self.doc.to_string()),
        };
        let info = self.gpu.info();
        let mode = self.mode();
        status::lines(&Status {
            document: name,
            kind,
            mode,
            zoom: match mode {
                Mode::Model => self.orbit.zoom,
                Mode::Canvas => self.canvas.zoom,
            },
            yaw: self.orbit.yaw,
            pitch: self.orbit.pitch,
            pan: match mode {
                Mode::Model => self.orbit.pan,
                Mode::Canvas => self.canvas.pan,
            },
            content: self.subject.content_size(),
            frame_ms: self.frame_ms,
            backend: info.backend.clone(),
            adapter: info.name.clone(),
            samples: self.renderer.as_ref().map(|r| r.samples()).unwrap_or(1),
            reloads: self.watch.reloads(),
        })
    }

    fn configure_surface(&mut self) {
        let (Some(surface), Some(config)) = (&self.surface, &mut self.config) else {
            return;
        };
        if !self.viewport.is_drawable() {
            return;
        }
        let [w, h] = self.viewport.physical();
        if config.width == w && config.height == h {
            return;
        }
        config.width = w;
        config.height = h;
        surface.configure(self.gpu.device(), config);
    }

    /// Re-read the project and rebuild whatever the document turned into. Called from
    /// the journal poll and from the `r` key.
    fn rebuild(&mut self, ws: Workspace) {
        self.ws = ws;
        let doc = match self.ws.project.resolve_doc(Some(self.doc.as_str())) {
            Ok(d) => d,
            // The document was deleted out from under the window. Keep showing the last
            // good frame rather than tearing the window down.
            Err(_) => return,
        };
        self.doc = doc;
        let loaded = subject::load(
            &self.ws,
            &self.doc,
            self.orbit.camera(),
            Lighting::default(),
            self.gpu.device().limits().max_texture_dimension_2d,
        );
        let Ok(subject) = loaded else { return };
        let was = self.subject.mode();
        self.subject = subject;
        if self.subject.mode() != was {
            self.orbit.reset();
            self.canvas.reset();
        }
        if let (Some(r), Some(_)) = (&mut self.renderer, self.subject.content_size()) {
            r.set_subject(&self.gpu, &self.subject);
        }
        if let Some(w) = &self.window {
            w.set_title(&self.title());
        }
        // The shared journal is the point of the tool, so say when it moved the window.
        eprintln!(
            "dpaint-view: reloaded {} at journal seq {}",
            self.doc,
            self.watch
                .seq()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "-".into())
        );
    }

    fn poll_journal(&mut self) {
        match self.watch.poll(Instant::now()) {
            Ok(Some(ws)) => self.rebuild(ws),
            Ok(None) => {}
            // A half-written project during another process's save is transient: the
            // next poll picks it up. Never fatal.
            Err(_) => {}
        }
    }

    fn act(&mut self, action: Action, el: &ActiveEventLoop) {
        match action {
            Action::Quit => el.exit(),
            Action::Frame => match self.mode() {
                Mode::Model => self.orbit.frame(),
                Mode::Canvas => {
                    if let Some(c) = self.subject.content_size() {
                        self.canvas.fit(c, self.viewport.physical());
                    }
                }
            },
            Action::Reset => {
                self.orbit.reset();
                self.canvas.reset();
            }
            Action::ToggleChecker => self.canvas.checker = !self.canvas.checker,
            Action::TogglePixelated => self.canvas.pixelated = !self.canvas.pixelated,
            Action::ToggleStatus => {
                self.show_status = !self.show_status;
                self.status_due = Instant::now();
            }
            Action::Reload => {
                self.watch.invalidate();
                self.poll_journal();
            }
        }
    }

    fn drag_to(&mut self, x: f32, y: f32) {
        let (dx, dy) = (x - self.cursor[0], y - self.cursor[1]);
        self.cursor = [x, y];
        let Some((_, drag)) = self.drag else { return };
        match (drag, self.mode()) {
            (Drag::Orbit, _) => self.orbit.orbit(dx, dy),
            (Drag::Pan, Mode::Model) => self.orbit.pan_by(dx, dy, self.viewport.height()),
            (Drag::Pan, Mode::Canvas) => self.canvas.pan_by(dx, dy),
            (Drag::None, _) => {}
        }
    }

    fn wheel(&mut self, w: Wheel) {
        let steps = input::notches(w);
        match self.mode() {
            Mode::Model => self.orbit.zoom_by(steps),
            Mode::Canvas => self
                .canvas
                .zoom_at(self.cursor, self.viewport.physical(), steps),
        }
    }

    fn redraw(&mut self) {
        let now = Instant::now();
        if let Some(prev) = self.last_frame {
            let ms = (now - prev).as_secs_f32() * 1000.0;
            self.frame_ms = if self.frame_ms > 0.0 {
                self.frame_ms * FRAME_SMOOTHING + ms * (1.0 - FRAME_SMOOTHING)
            } else {
                ms
            };
        }
        self.last_frame = Some(now);

        if !self.viewport.is_drawable() {
            return;
        }
        let Some(surface) = &self.surface else { return };

        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                if let (Some(s), Some(c)) = (&self.surface, &self.config) {
                    s.configure(self.gpu.device(), c);
                }
                return;
            }
            // Timed out, occluded, or a validation error already reported through the
            // error scope: skip the frame and try the next one.
            _ => return,
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let size = self.viewport.physical();
        let scale = self.viewport.scale() as f32;

        if let Some(scene) = self.subject.scene_mut() {
            scene.camera = self.orbit.camera();
            scene.pan = self.orbit.pan;
        }
        // The readout carries a frame-time counter, so its text changes every single
        // frame. Rasterizing and re-uploading a panel sixty times a second to move one
        // digit is the most expensive thing a still viewport could do, and a number
        // that flickers that fast is unreadable anyway: refresh it a few times a second.
        let refresh_status = self.show_status && now >= self.status_due;
        if refresh_status {
            self.status_due = now + STATUS_INTERVAL;
        }
        let lines = if refresh_status {
            Some(self.status_lines())
        } else {
            None
        };
        let state = self.canvas.view_state();
        if let Some(r) = &mut self.renderer {
            if let Some(l) = &lines {
                r.set_status(&self.gpu, l, scale);
            }
            if !self.show_status {
                r.clear_status();
            }
            r.draw(&self.gpu, &view, size, &self.subject, state, scale);
        }
        self.gpu.queue().present(frame);
    }
}

impl ApplicationHandler for ViewApp {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(self.title())
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 800.0));
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.fatal = Some(format!("could not open a window: {e}"));
                el.exit();
                return;
            }
        };
        let surface = match self.gpu.instance().create_surface(window.clone()) {
            Ok(s) => s,
            Err(e) => {
                self.fatal = Some(format!(
                    "could not create a GPU surface for the window: {e}"
                ));
                el.exit();
                return;
            }
        };

        let size = window.inner_size();
        self.viewport = Viewport::from_physical([size.width, size.height], window.scale_factor());
        let [w, h] = self.viewport.physical();
        let Some(mut config) = surface.get_default_config(self.gpu.adapter(), w.max(1), h.max(1))
        else {
            self.fatal =
                Some("this GPU adapter cannot present to a window on this platform".into());
            el.exit();
            return;
        };
        // dpaint-gpu's shaders sRGB-encode in the fragment stage, matching the CPU
        // renderer. An sRGB surface format would encode a second time.
        let caps = surface.get_capabilities(self.gpu.adapter());
        if let Some(linear) = caps.formats.iter().copied().find(|f| !f.is_srgb()) {
            config.format = linear;
        }
        surface.configure(self.gpu.device(), &config);

        let samples = if self.samples > 1 && self.gpu.supports_msaa4(config.format) {
            self.samples
        } else {
            1
        };
        let mut renderer = ViewRenderer::new(&self.gpu, config.format, samples);
        renderer.set_subject(&self.gpu, &self.subject);
        if let Some(content) = self.subject.content_size() {
            self.canvas.fit(content, self.viewport.physical());
        }

        self.config = Some(config);
        self.surface = Some(surface);
        self.renderer = Some(renderer);

        // Launched from a terminal, so say what opened and on what. It is also the
        // difference between "the window is up" and "the window never appeared" when
        // something goes wrong with the display.
        let info = self.gpu.info();
        eprintln!(
            "dpaint-view: {} · {} — {}x{} @{:.1}x, {}x MSAA, {} · {}",
            self.ws
                .project
                .doc(&self.doc)
                .map(|d| d.name().to_string())
                .unwrap_or_else(|_| self.doc.to_string()),
            self.subject_kind(),
            self.viewport.physical()[0],
            self.viewport.physical()[1],
            self.viewport.scale(),
            samples,
            info.backend,
            info.name
        );
        window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),

            WindowEvent::Resized(size) => {
                self.viewport.resize([size.width, size.height]);
                self.configure_surface();
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.viewport.set_scale(scale_factor);
                if let Some(w) = &self.window {
                    let size = w.inner_size();
                    self.viewport.resize([size.width, size.height]);
                }
                self.configure_surface();
            }

            WindowEvent::ModifiersChanged(m) => {
                let s = m.state();
                self.mods = Mods {
                    shift: s.shift_key(),
                    ctrl: s.control_key(),
                    alt: s.alt_key(),
                    logo: s.super_key(),
                };
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.drag_to(position.x as f32, position.y as f32);
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let Some(b) = map_button(button) else { return };
                match state {
                    ElementState::Pressed => {
                        self.drag = Some((b, input::drag_for(self.mode(), b, self.mods)));
                    }
                    ElementState::Released => {
                        if self.drag.map(|(held, _)| held == b).unwrap_or(false) {
                            self.drag = None;
                        }
                    }
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                self.wheel(match delta {
                    MouseScrollDelta::LineDelta(_, y) => Wheel::Lines(y),
                    MouseScrollDelta::PixelDelta(p) => Wheel::Pixels(p.y as f32),
                });
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed || event.repeat {
                    return;
                }
                let key = match event.logical_key.as_ref() {
                    Key::Named(NamedKey::Escape) => "escape".to_string(),
                    Key::Character(c) => c.to_lowercase(),
                    _ => return,
                };
                if let Some(a) = input::action_for(&key) {
                    self.act(a, el);
                }
            }

            WindowEvent::RedrawRequested => self.redraw(),

            _ => {}
        }
    }

    fn about_to_wait(&mut self, _el: &ActiveEventLoop) {
        self.poll_journal();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

fn map_button(b: MouseButton) -> Option<Button> {
    match b {
        MouseButton::Left => Some(Button::Left),
        MouseButton::Right => Some(Button::Right),
        MouseButton::Middle => Some(Button::Middle),
        _ => None,
    }
}

/// Run the window to completion. Returns an error when the event loop failed or the app
/// hit something it could not recover from.
pub fn run(mut app: ViewApp) -> anyhow::Result<()> {
    let el = EventLoop::new()?;
    // Poll rather than Wait: the journal is polled between frames, and a redraw is
    // requested every iteration so the surface's Fifo present mode is what paces us.
    el.set_control_flow(ControlFlow::Poll);
    el.run_app(&mut app)?;
    match app.fatal() {
        Some(e) => Err(anyhow::anyhow!("{e}")),
        None => Ok(()),
    }
}
