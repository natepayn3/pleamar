//! Wayland: one layer-shell surface per monitor, its scale —the fractional
//! one too— and the mouse. It's the only part of the program that knows what Wayland is.

use super::PlatformWindow;
use crate::scene::*;
use crate::gpu;
use raw_window_handle::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
use smithay_client_toolkit::data_device_manager::{
    data_device::{DataDevice, DataDeviceData, DataDeviceHandler},
    data_offer::{DataOfferHandler, DragOffer},
    data_source::DataSourceHandler,
    DataDeviceManagerState, WritePipe,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers, RepeatInfo},
        pointer::{cursor_shape::CursorShapeManager, PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
        xdg::{
            popup::{Popup, PopupConfigure, PopupHandler},
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
            XdgPositioner, XdgShell,
        },
        WaylandSurface,
    },
};
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::Sender;
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::reexports::protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};
use wayland_protocols::ext::background_effect::v1::client::{ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1, ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1};

/// Whoever blurs what's behind a surface, if the compositor knows how to do
/// it (`ext-background-effect`): Hyprland and KWin, yes. Without it, the glass
/// is a tint with its light, with nothing blurry behind.
static BACKGROUND_EFFECTS: std::sync::OnceLock<ExtBackgroundEffectManagerV1> = std::sync::OnceLock::new();

use wayland_protocols_wlr::screencopy::v1::client::{zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1}, zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1};
use wayland_client::protocol::{wl_buffer::WlBuffer, wl_shm::{self, WlShm}, wl_shm_pool::WlShmPool};
use smithay_client_toolkit::reexports::protocols::wp::linux_dmabuf::zv1::client::{zwp_linux_buffer_params_v1, zwp_linux_dmabuf_feedback_v1::{self, ZwpLinuxDmabufFeedbackV1}, zwp_linux_dmabuf_v1::{self, ZwpLinuxDmabufV1}};

/// Whoever takes frames on the card from us (`linux-dmabuf`): with it a
/// surface is painted in frames of its own, and the compositor is told
/// which piece of each changed. And the layouts it reads ARGB8888 in.
struct Dmabufs {
    global: ZwpLinuxDmabufV1,
    qh: QueueHandle<State>,
    connection: Connection,
}
static DMABUFS: std::sync::OnceLock<Dmabufs> = std::sync::OnceLock::new();
static ARGB_LAYOUTS: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// Version 3 says each format and layout it reads, one by one.
struct Layouts;
impl Dispatch2<ZwpLinuxDmabufV1, State> for Layouts {
    fn event(&self, _: &mut State, _: &ZwpLinuxDmabufV1, ev: zwp_linux_dmabuf_v1::Event, _: &Connection, _: &QueueHandle<State>) {
        if let zwp_linux_dmabuf_v1::Event::Modifier { format, modifier_hi, modifier_lo } = ev {
            let m = ((modifier_hi as u64) << 32) | modifier_lo as u64;
            let mut all = ARGB_LAYOUTS.lock().unwrap();
            if format == crate::dmabuf::ARGB && !all.contains(&m) {
                all.push(m);
            }
        }
    }
}

/// Version 4 says which card the compositor paints with: ours is chosen to be
/// the same (see `gpu::compositor_card`).
struct MainDevice;
impl Dispatch2<ZwpLinuxDmabufFeedbackV1, State> for MainDevice {
    fn event(&self, _: &mut State, feedback: &ZwpLinuxDmabufFeedbackV1, ev: zwp_linux_dmabuf_feedback_v1::Event, _: &Connection, _: &QueueHandle<State>) {
        match ev {
            zwp_linux_dmabuf_feedback_v1::Event::MainDevice { device } => {
                if let Ok(dev) = <[u8; 8]>::try_from(device.as_slice()) {
                    crate::gpu::compositor_card(u64::from_ne_bytes(dev));
                }
            }
            zwp_linux_dmabuf_feedback_v1::Event::Done => feedback.destroy(),
            _ => {}
        }
    }
}

/// A frame of ours the compositor has stopped reading: it can be painted in again.
struct Released(Arc<std::sync::atomic::AtomicBool>);
impl Dispatch2<WlBuffer, State> for Released {
    fn event(&self, _: &mut State, _: &WlBuffer, ev: wayland_client::protocol::wl_buffer::Event, _: &Connection, _: &QueueHandle<State>) {
        if let wayland_client::protocol::wl_buffer::Event::Release = ev {
            self.0.store(false, Ordering::Release);
        }
    }
}

/// A surface's frames, ours: three, painted in turn —the one on screen, one
/// on its way, one to paint—. Each is handed over once the card has painted
/// it, with only the pieces that changed damaged, from a thread of its own so
/// the render does not wait for the card.
struct OwnFrames {
    wl: wl_surface::WlSurface,
    px: (u32, u32),
    frames: Vec<OwnFrame>,
    latest: Option<usize>,
    failed: bool,
    to_show: Option<Sender<Show>>,
    /// The same queue, for the window's own commits (see `WaylandWindow::shown`).
    shared: Arc<Mutex<Option<Sender<Show>>>>,
}

struct OwnFrame {
    texture: wgpu::Texture,
    buffer: WlBuffer,
    /// On screen or on its way there: not to be painted in.
    busy: Arc<std::sync::atomic::AtomicBool>,
    fd: std::os::fd::OwnedFd,
}

/// Whether nobody is still reading or writing that frame on the card, waiting
/// for it up to `wait` (none: just asking). A compositor lets go of a frame
/// (`wl_buffer.release`) once it has asked the card to read it, not once the
/// card has: the kernel keeps that read on the buffer, and a program that
/// paints with the system's own frames waits for it without knowing. Ours are
/// painted by Vulkan as plain memory, which does not wait: painted in at
/// once, on Hyprland with an AMD card, a frame was being brought up to date
/// while the compositor was still putting it on the screen, and pieces of an
/// older one showed through, flickering. Without the kernel's answer
/// (before Linux 6.0), it is taken as free.
fn card_done_with(fd: &std::os::fd::OwnedFd, wait: std::time::Duration) -> bool {
    #[repr(C)]
    struct ExportSyncFile {
        flags: u32,
        fd: i32,
    }
    // DMA_BUF_IOCTL_EXPORT_SYNC_FILE, asking for what a write has to wait for:
    // every read and write still pending.
    const EXPORT_SYNC_FILE: libc::c_ulong = 0xC008_6202;
    const SYNC_WRITE: u32 = 2;
    let mut arg = ExportSyncFile { flags: SYNC_WRITE, fd: -1 };
    // SAFETY: a dmabuf of ours and the structure the call fills.
    let r = unsafe { libc::ioctl(std::os::fd::AsRawFd::as_raw_fd(fd), EXPORT_SYNC_FILE, &mut arg) };
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| match r {
        0 => println!("render · our frames are painted in again once the card has finished reading them"),
        _ => println!("render · the kernel does not say when the card has finished reading our frames ({}): they are painted in again when the compositor lets go of them", std::io::Error::last_os_error()),
    });
    if r != 0 || arg.fd < 0 {
        return true;
    }
    // SAFETY: the call has just made this file, and it is ours to close.
    let fence = unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(arg.fd) };
    let mut p = libc::pollfd { fd: std::os::fd::AsRawFd::as_raw_fd(&fence), events: libc::POLLIN, revents: 0 };
    // SAFETY: one pollfd of ours.
    let n = unsafe { libc::poll(&mut p, 1, wait.as_millis().min(i32::MAX as u128) as i32) };
    n > 0
}

enum Show {
    /// That one, once the card has painted it: those pieces changed (`None`, all).
    Frame { buffer: WlBuffer, px: (u32, u32), pieces: Option<Vec<[u32; 4]>>, sent: gpu::Sent },
    /// These are not used any more: after whatever was asked before.
    Forget(Vec<WlBuffer>),
    /// Say so once everything asked before has reached the compositor.
    Settled(std::sync::mpsc::Sender<()>),
    /// A commit of what the window has changed meanwhile (its input region, its
    /// keyboard), after the frames asked before it.
    Commit,
}

const OWN_FRAMES: usize = 3;

impl OwnFrames {
    fn forget(&mut self) {
        let buffers: Vec<WlBuffer> = self.frames.drain(..).map(|f| f.buffer).collect();
        self.latest = None;
        if buffers.is_empty() {
            return;
        }
        match &self.to_show {
            Some(tx) => {
                let _ = tx.send(Show::Forget(buffers));
            }
            None => buffers.iter().for_each(|b| b.destroy()),
        }
    }

    fn make(&mut self, device: &wgpu::Device) -> Result<(), String> {
        let d = DMABUFS.get().ok_or("the compositor takes no frames on the card")?;
        let layouts = ARGB_LAYOUTS.lock().unwrap().clone();
        for _ in 0..OWN_FRAMES {
            let e = crate::dmabuf::export(device, self.px, &layouts)?;
            let params = d.global.create_params(&d.qh, Silent);
            for (k, (offset, stride)) in e.planes.iter().enumerate() {
                params.add(std::os::fd::AsFd::as_fd(&e.fd), k as u32, *offset, *stride, (e.modifier >> 32) as u32, e.modifier as u32);
            }
            let busy = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let buffer = params.create_immed(self.px.0 as i32, self.px.1 as i32, crate::dmabuf::ARGB, zwp_linux_buffer_params_v1::Flags::empty(), &d.qh, Released(busy.clone()));
            params.destroy();
            self.frames.push(OwnFrame { texture: e.texture, buffer, busy, fd: e.fd });
        }
        let _ = d.connection.flush();
        println!("render · {} frames of our own of {}×{} handed to the compositor, with what changed in each", self.frames.len(), self.px.0, self.px.1);
        Ok(())
    }
}

impl gpu::Frames for OwnFrames {
    fn acquire(&mut self, device: &wgpu::Device, _modifiers: &[u64]) -> Option<(usize, wgpu::Texture)> {
        if self.failed {
            return None;
        }
        if self.frames.is_empty()
            && let Err(e) = self.make(device)
        {
            println!("render · no frames of our own ({e}): the system's swapchain, which tells the compositor all of it changed each time");
            self.forget();
            self.failed = true;
            return None;
        }
        // Not the one just handed over, which may not be on screen yet; of
        // the others, one the card has finished reading (see `card_done_with`),
        // or else the first let go of, once the card is done with it.
        let free: Vec<usize> = (0..self.frames.len()).filter(|k| Some(*k) != self.latest && !self.frames[*k].busy.load(Ordering::Acquire)).collect();
        let k = match free.iter().copied().find(|k| card_done_with(&self.frames[*k].fd, std::time::Duration::ZERO)) {
            Some(k) => k,
            None => {
                let k = *free.first()?;
                card_done_with(&self.frames[k].fd, std::time::Duration::from_millis(100));
                k
            }
        };
        Some((k, self.frames[k].texture.clone()))
    }

    fn present(&mut self, which: usize, done: wgpu::SubmissionIndex, device: &wgpu::Device, queue: &wgpu::Queue, changed: Option<[u32; 4]>) {
        self.present_pieces(which, done, device, queue, changed.map(|c| vec![c]).as_deref());
    }

    fn present_pieces(&mut self, which: usize, _done: wgpu::SubmissionIndex, device: &wgpu::Device, queue: &wgpu::Queue, pieces: Option<&[[u32; 4]]>) {
        let Some(f) = self.frames.get(which) else { return };
        f.busy.store(true, Ordering::Release);
        self.latest = Some(which);
        let tx = self.to_show.get_or_insert_with(|| {
            let (tx, rx) = std::sync::mpsc::channel::<Show>();
            let (wl, device) = (self.wl.clone(), device.clone());
            let connection = DMABUFS.get().map(|d| d.connection.clone());
            let _ = std::thread::Builder::new().name("frames shown".into()).spawn(move || {
                for job in rx {
                    match job {
                        Show::Frame { buffer, px, pieces, sent } => {
                            // The compositor reads it as it is: not before the card has finished it.
                            sent.wait(&device, std::time::Duration::from_secs(1));
                            wl.attach(Some(&buffer), 0, 0);
                            match pieces {
                                None => wl.damage_buffer(0, 0, px.0 as i32, px.1 as i32),
                                Some(p) => {
                                    for [x, y, w, h] in p.into_iter().filter(|b| b[2] > 0 && b[3] > 0) {
                                        wl.damage_buffer(x as i32, y as i32, w as i32, h as i32);
                                    }
                                }
                            }
                            wl.commit();
                        }
                        Show::Forget(buffers) => buffers.iter().for_each(|b| b.destroy()),
                        Show::Commit => wl.commit(),
                        Show::Settled(done) => {
                            if let Some(c) = &connection {
                                let _ = c.flush();
                            }
                            let _ = done.send(());
                        }
                    }
                    if let Some(c) = &connection {
                        let _ = c.flush();
                    }
                }
            });
            *self.shared.lock().unwrap() = Some(tx.clone());
            tx
        });
        let _ = tx.send(Show::Frame { buffer: f.buffer.clone(), px: self.px, pieces: pieces.map(|p| p.to_vec()), sent: gpu::Sent::after(queue) });
    }

    fn whole_screen(&self) -> bool {
        false
    }

    /// A glass's capture of what is behind is asked for right after a frame:
    /// asked before the frame reached the compositor, it showed the one
    /// before, and the lens unmixed the wrong picture.
    fn settle(&mut self) {
        let Some(tx) = &self.to_show else { return };
        let (done, wait) = std::sync::mpsc::channel();
        if tx.send(Show::Settled(done)).is_ok() {
            let _ = wait.recv_timeout(std::time::Duration::from_secs(1));
        }
    }

    fn resize(&mut self, px: (u32, u32)) {
        if px != self.px {
            self.forget();
            self.px = px;
        }
    }

    fn gave_up(&self) -> bool {
        self.failed
    }
}

impl Drop for OwnFrames {
    fn drop(&mut self) {
        self.forget();
        // Back to the system's frames: the window commits by itself again.
        *self.shared.lock().unwrap() = None;
    }
}

/// Whoever photographs the screen (`wlr-screencopy`), and the shared memory
/// where it leaves the capture. With both, pleamar can see what's behind it.
static SCREENCOPY: std::sync::OnceLock<(ZwlrScreencopyManagerV1, WlShm)> = std::sync::OnceLock::new();

/// What's needed to capture what's behind a surface: which
/// monitor it's on and where inside it —in logical pixels—, and whether there's
/// already a capture on the way. The position is set by the Wayland thread when configuring it.
struct BackdropTarget {
    id: u32,
    /// Which monitor it's on, and what it's called. A window knows it when it enters it.
    output: Mutex<Option<(wl_output::WlOutput, String)>>,
    /// Where it is on its monitor, and its size, in logical pixels.
    position: Mutex<Option<(i32, i32)>>,
    size: Mutex<(u32, u32)>,
    kind: BackdropKind,
    /// When the compositor was last asked where it is.
    last_asked: Mutex<Option<std::time::Instant>>,
    /// With what opacity the compositor blends its content: a window may not be fully opaque.
    opacity: Mutex<f32>,
    in_flight: std::sync::atomic::AtomicBool,
    /// The captures on the way (one per part of the box), so they can be forgotten.
    frames: Mutex<Vec<ZwlrScreencopyFrameV1>>,
}

/// How to know where a surface is on its monitor.
enum BackdropKind {
    /// A layer: by the layer-shell rules, and if it's Hyprland, by asking it.
    Layer,
    /// A normal window: only by asking, since anyone can move it.
    Window,
    /// A popup: wherever its parent is, plus where the compositor put it.
    Popup { parent: Arc<BackdropTarget>, offset: Mutex<(i32, i32)> },
}

impl BackdropTarget {
    fn new(id: u32, output: Option<(wl_output::WlOutput, String)>, kind: BackdropKind) -> Arc<BackdropTarget> {
        Arc::new(BackdropTarget { id, output: Mutex::new(output), position: Mutex::new(None), size: Mutex::new((0, 0)), kind, last_asked: Mutex::new(None), opacity: Mutex::new(1.0), in_flight: std::sync::atomic::AtomicBool::new(false), frames: Mutex::new(Vec::new()) })
    }

    /// On which monitor and where, now. With Hyprland it gets asked, at most
    /// once per second: a window moves without anyone reconfiguring it,
    /// and a layer gets pushed aside by a bar that arrives later.
    fn output_and_position(&self) -> Option<(wl_output::WlOutput, (i32, i32))> {
        if let BackdropKind::Popup { parent, offset } = &self.kind {
            let (output, (x, y)) = parent.output_and_position()?;
            let r = *offset.lock().unwrap();
            return Some((output, (x + r.0, y + r.1)));
        }
        let due = self.last_asked.lock().unwrap().is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(1));
        if due && super::hyprland::is_present() {
            *self.last_asked.lock().unwrap() = Some(std::time::Instant::now());
            let size = *self.size.lock().unwrap();
            match self.kind {
                BackdropKind::Layer => {
                    let name = self.output.lock().unwrap().as_ref().map(|s| s.1.clone());
                    if let Some(position) = name.and_then(|n| super::hyprland::layer_position(&n, size)) {
                        *self.position.lock().unwrap() = Some(position);
                    }
                }
                BackdropKind::Window => {
                    if let Some((monitor, position, opacity)) = super::hyprland::window_position(size) {
                        *self.opacity.lock().unwrap() = opacity;
                        if let Some(o) = OUTPUTS.lock().unwrap().get(&monitor) {
                            *self.output.lock().unwrap() = Some((o.clone(), monitor));
                        }
                        *self.position.lock().unwrap() = Some(position);
                    }
                }
                BackdropKind::Popup { .. } => {}
            }
        }
        let output = self.output.lock().unwrap().as_ref()?.0.clone();
        Some((output, (*self.position.lock().unwrap())?))
    }
}

/// The monitors by their name: a window that moves from one to another is
/// photographed on the one the compositor says.
static OUTPUTS: std::sync::LazyLock<Mutex<std::collections::HashMap<String, wl_output::WlOutput>>> = std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));
/// When a monitor last came or went. While they are changing (a wake from deep
/// sleep, where Hyprland's stand-in FALLBACK comes and goes as the real ones
/// return), a monitor can be gone in the compositor before this side has heard
/// so, and photographing it then is a fatal protocol error: no backdrop is
/// photographed until they have been still for a moment.
static OUTPUTS_CHANGED: std::sync::LazyLock<Mutex<Option<std::time::Instant>>> = std::sync::LazyLock::new(|| Mutex::new(None));
const OUTPUTS_SETTLE: std::time::Duration = std::time::Duration::from_millis(1500);

/// A monitor came or went: backdrops wait until they have been still for
/// `OUTPUTS_SETTLE`, and then the render is woken, so that a glass whose
/// capture was refused meanwhile gets a fresh one even if nothing else moves.
fn outputs_changed(to_render: &Sender<ToRender>) {
    *OUTPUTS_CHANGED.lock().unwrap() = Some(std::time::Instant::now());
    let tx = to_render.clone();
    std::thread::spawn(move || {
        std::thread::sleep(OUTPUTS_SETTLE + std::time::Duration::from_millis(50));
        let _ = tx.send(ToRender::Repaint);
    });
}

/// Where a layer lands inside its monitor, by the layer-shell rules: the
/// margin only counts on the side it's stuck to, and what isn't stuck to
/// any side goes centred. It doesn't know about the zones another layer reserves (a
/// bar that pushes the others aside): with one of those, it's off by its height.
fn position_on_output(anchor: Anchor, margin: [i32; 4], size: (u32, u32), output: (i32, i32)) -> (i32, i32) {
    let (w, h) = (size.0 as i32, size.1 as i32);
    let x = match (anchor.contains(Anchor::LEFT), anchor.contains(Anchor::RIGHT)) {
        (true, true) => margin[3] + (output.0 - margin[3] - margin[1] - w) / 2,
        (true, false) => margin[3],
        (false, true) => output.0 - w - margin[1],
        (false, false) => (output.0 - w) / 2,
    };
    let y = match (anchor.contains(Anchor::TOP), anchor.contains(Anchor::BOTTOM)) {
        (true, true) => margin[0] + (output.1 - margin[0] - margin[2] - h) / 2,
        (true, false) => margin[0],
        (false, true) => output.1 - h - margin[2],
        (false, false) => (output.1 - h) / 2,
    };
    (x, y)
}

/// A capture on the way: whose, and which box.
struct CaptureFor {
    target: Arc<BackdropTarget>,
    on_change: bool,
    /// What the compositor offers to copy it into: size, stride and format.
    format: Mutex<Option<(u32, u32, u32, wl_shm::Format)>>,
    /// A capture of several small boxes instead of one big one: which of them this is, and what they share.
    part: Option<(Arc<Mutex<Parts>>, usize)>,
}

/// A capture made of several small ones: the glass of a shell is a frame round the screen, and the one box
/// that holds it all is the whole screen, of which the compositor would read back (and we would upload) twenty
/// times what is needed. Each part is photographed on its own and copied to where it goes in one buffer
/// the size of the whole box, which reaches the render as any other capture, with the list of what in it is real.
struct Parts {
    /// The whole box, and the parts, logical, `[x, y, width, height]` in the window's coordinates.
    hull: [i32; 4],
    rects: Vec<[i32; 4]>,
    /// The parts that have arrived, in real pixels of the assembled buffer.
    valid: Vec<[u32; 4]>,
    /// It has been sent (or failed): what still arrives is forgotten.
    over: bool,
}

/// Set when the parts of a capture did not fit together (a size the compositor rounded another way): from
/// then on the box is captured whole, as it always was.
static PARTS_BROKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The assembled buffer of a surface, and its size in real pixels.
struct Assembled {
    size: (u32, u32),
    map: Arc<MappedMemory>,
}

/// The memory where the compositor leaves a surface's captures. It's
/// reused as long as it has the same size.
struct CaptureBuffer {
    map: Arc<MappedMemory>,
    size: (u32, u32, u32, wl_shm::Format),
    _pool: WlShmPool,
    buffer: WlBuffer,
    _fd: std::os::fd::OwnedFd,
}

impl Drop for CaptureBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

/// A capture's memory, as seen from this process. It travels to the render without
/// being copied: the compositor doesn't write to it again until it's asked for
/// another capture, and the render only asks for it after having uploaded it to the card.
struct MappedMemory {
    ptr: *mut u8,
    len: usize,
}
unsafe impl Send for MappedMemory {}
unsafe impl Sync for MappedMemory {}
impl AsRef<[u8]> for MappedMemory {
    fn as_ref(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}
impl Drop for MappedMemory {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len) };
    }
}

impl CaptureFor {
    /// Every capture of this surface still on the way is forgotten, this one among them, and a new one may be asked for.
    fn finish(&self) {
        for f in self.target.frames.lock().unwrap().drain(..) {
            f.destroy();
        }
        self.target.in_flight.store(false, Ordering::Relaxed);
    }

    fn failed(&self, e: &State) {
        let _ = e.to_render.send(ToRender::Backdrop(Box::new(super::Backdrop { sheet: self.target.id, width: 0, height: 0, stride: 0, opacity: 1.0, data: None, rects: Vec::new() })));
    }
}

impl Dispatch2<ZwlrScreencopyFrameV1, State> for CaptureFor {
    fn event(&self, e: &mut State, frame: &ZwlrScreencopyFrameV1, ev: zwlr_screencopy_frame_v1::Event, _: &Connection, qh: &QueueHandle<State>) {
        use zwlr_screencopy_frame_v1::Event as E;
        let part = self.part.as_ref().map_or(0, |p| p.1);
        match ev {
            // Only in the usual format: four bytes, blue first, and the buffer
            // in the very format offered (only its colours are read: the
            // fourth byte does not matter). Made always as ARGB, niri, which
            // offers XRGB, called it an invalid buffer and closed the program
            // (issue 50).
            E::Buffer { format: wayland_client::WEnum::Value(f), width, height, stride } if matches!(f, wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888) => {
                *self.format.lock().unwrap() = Some((width, height, stride, f));
            }
            E::BufferDone => {
                let (Some(size), Some((_, shm))) = (*self.format.lock().unwrap(), SCREENCOPY.get()) else {
                    self.finish();
                    return;
                };
                let key = (self.target.id, part);
                if e.capture_buffers.get(&key).is_none_or(|f| f.size != size) {
                    e.capture_buffers.remove(&key);
                    let len = (size.2 * size.1) as usize;
                    let Some(fd) = memfd(len) else { return self.finish() };
                    let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, std::os::fd::AsRawFd::as_raw_fd(&fd), 0) };
                    if ptr == libc::MAP_FAILED {
                        return self.finish();
                    }
                    let pool = shm.create_pool(std::os::fd::AsFd::as_fd(&fd), len as i32, qh, Silent);
                    let buffer = pool.create_buffer(0, size.0 as i32, size.1 as i32, size.2 as i32, size.3, qh, Silent);
                    e.capture_buffers.insert(key, CaptureBuffer { map: Arc::new(MappedMemory { ptr: ptr as *mut u8, len }), size, _pool: pool, buffer, _fd: fd });
                }
                // When watching, it waits for something to change: with what's behind
                // standing still, nothing arrives. After presenting, it brings it the next time it's painted.
                if self.on_change {
                    frame.copy_with_damage(&e.capture_buffers[&key].buffer);
                } else {
                    frame.copy(&e.capture_buffers[&key].buffer);
                }
            }
            E::Ready { .. } => {
                let id = self.target.id;
                let opacity = *self.target.opacity.lock().unwrap();
                let Some((parts, k)) = &self.part else {
                    if let Some(f) = e.capture_buffers.get(&(id, 0)) {
                        let data: Arc<dyn AsRef<[u8]> + Send + Sync> = f.map.clone();
                        let _ = e.to_render.send(ToRender::Backdrop(Box::new(super::Backdrop { sheet: id, width: f.size.0, height: f.size.1, stride: f.size.2, opacity, data: Some(data), rects: Vec::new() })));
                    }
                    return self.finish();
                };
                let mut ps = parts.lock().unwrap();
                if ps.over {
                    return;
                }
                let (Some(size), Some(src)) = (*self.format.lock().unwrap(), e.capture_buffers.get(&(id, *k)).map(|f| f.map.clone())) else {
                    ps.over = true;
                    self.failed(e);
                    return self.finish();
                };
                // Where this part goes in the whole box, in the real pixels the compositor answered in.
                let (r, hull) = (ps.rects[*k], ps.hull);
                let scale = size.0 as f32 / r[2].max(1) as f32;
                let whole = ((hull[2] as f32 * scale).round() as u32, (hull[3] as f32 * scale).round() as u32);
                let at = (((r[0] - hull[0]) as f32 * scale).round() as u32, ((r[1] - hull[1]) as f32 * scale).round() as u32);
                if at.0 + size.0 > whole.0 || at.1 + size.1 > whole.1 || (size.1 as f32 - r[3] as f32 * scale).abs() > 1.0 {
                    eprintln!("render · the parts of a capture do not fit together ({size:?} at {at:?} in {whole:?}): capturing the whole box from now on");
                    PARTS_BROKEN.store(true, Ordering::Relaxed);
                    ps.over = true;
                    self.failed(e);
                    return self.finish();
                }
                if e.capture_hulls.get(&id).is_none_or(|h| h.size != whole) {
                    let len = whole.0 as usize * whole.1 as usize * 4;
                    // Anonymous memory, not shared with anyone: only the render reads it.
                    let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0) };
                    if ptr == libc::MAP_FAILED {
                        ps.over = true;
                        self.failed(e);
                        return self.finish();
                    }
                    e.capture_hulls.insert(id, Assembled { size: whole, map: Arc::new(MappedMemory { ptr: ptr as *mut u8, len }) });
                }
                let hull_map = e.capture_hulls[&id].map.clone();
                let (row, to) = (size.0 as usize * 4, whole.0 as usize * 4);
                for y in 0..size.1 as usize {
                    // SAFETY: inside both buffers (checked above); the render does not read the whole one while a capture is on the way.
                    unsafe { std::ptr::copy_nonoverlapping(src.ptr.add(y * size.2 as usize), hull_map.ptr.add((at.1 as usize + y) * to + at.0 as usize * 4), row) };
                }
                ps.valid.push([at.0, at.1, size.0, size.1]);
                // Watching, the first part that changed is enough: the others are forgotten, and the render, which paints
                // again for it, asks for everything right after presenting.
                if self.on_change || ps.valid.len() == ps.rects.len() {
                    ps.over = true;
                    let data: Arc<dyn AsRef<[u8]> + Send + Sync> = hull_map;
                    let _ = e.to_render.send(ToRender::Backdrop(Box::new(super::Backdrop { sheet: id, width: whole.0, height: whole.1, stride: whole.0 * 4, opacity, data: Some(data), rects: ps.valid.clone() })));
                    drop(ps);
                    self.finish();
                }
            }
            // So the render doesn't stay waiting for it.
            E::Failed => {
                if let Some((parts, _)) = &self.part {
                    let mut ps = parts.lock().unwrap();
                    if ps.over {
                        return;
                    }
                    ps.over = true;
                }
                self.failed(e);
                self.finish()
            }
            _ => {}
        }
    }
}

fn memfd(len: usize) -> Option<std::os::fd::OwnedFd> {
    unsafe {
        let fd = libc::memfd_create(c"pleamar-backdrop".as_ptr(), libc::MFD_CLOEXEC);
        if fd < 0 || libc::ftruncate(fd, len as i64) != 0 {
            return None;
        }
        Some(std::os::fd::FromRawFd::from_raw_fd(fd))
    }
}
use wayland_client::protocol::wl_data_device_manager::DndAction;
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_seat, wl_surface},
    Connection, Proxy, QueueHandle,
};


/// What the render needs from a Wayland surface, and nothing else.
/// Every surface's number —layers, popups, lock faces— from one counter:
/// three counters in ranges of a thousand met after the thousandth popup, and
/// the render could not tell which surface a message was for.
static SURFACE_IDS: AtomicU32 = AtomicU32::new(0);

fn next_surface_id() -> u32 {
    SURFACE_IDS.fetch_add(1, Ordering::Relaxed)
}

struct WaylandWindow {
    wl: wl_surface::WlSurface,
    compositor: CompositorState,
    /// To change the cursor you need the pointer's "device" and the serial
    /// number of the last time it entered: both are set by the Wayland thread.
    cursors: Arc<Mutex<Option<WpCursorShapeDeviceV1>>>,
    serial: Arc<AtomicU32>,
    /// A popup doesn't have one: the keyboard is its parent's business.
    layer: Option<LayerSurface>,
    /// To ask for the "you can paint another one now" notice: with which queue, and whose it is.
    qh: QueueHandle<State>,
    id: u32,
    /// Its blur, which is requested the first time it has glass: requesting it twice
    /// for the same surface is a protocol error.
    effect: Mutex<Option<ExtBackgroundEffectSurfaceV1>>,
    /// To see what it has behind. Only layers: for a window it's not known where it is.
    backdrop: Option<Arc<BackdropTarget>>,
    /// With frames of its own, the queue that hands them to the compositor:
    /// its other commits go through it too, after the frames asked before.
    /// Committed straight away, the input region's first commit overtook the
    /// first frame, still waiting for the card, and a lock screen with no
    /// buffer was killed by Hyprland (`Null buffer attached`, issue 49).
    shown: Arc<Mutex<Option<Sender<Show>>>>,
}

impl WaylandWindow {
    /// What it has changed, committed: in its turn after the frames, if it has frames of its own.
    fn commit(&self) {
        match self.shown.lock().unwrap().as_ref() {
            Some(tx) => {
                let _ = tx.send(Show::Commit);
            }
            None => {
                self.wl.commit();
                if let Some(c) = MOVABLE_LAYERS.get() {
                    let _ = c.connection.flush();
                }
            }
        }
    }
}

fn keyboard_interactivity(t: Keyboard) -> KeyboardInteractivity {
    match t {
        Keyboard::Never => KeyboardInteractivity::None,
        Keyboard::OnDemand => KeyboardInteractivity::OnDemand,
        Keyboard::Always => KeyboardInteractivity::Exclusive,
    }
}

impl PlatformWindow for WaylandWindow {
    fn update_input_region(&self, boxes: &[[i32; 4]]) {
        if let Ok(region) = Region::new(&self.compositor) {
            for c in boxes {
                region.add(c[0], c[1], c[2] - c[0], c[3] - c[1]);
            }
            // Committed now, not with the next frame: a surface that has just
            // closed paints no more frames, and its old region stayed on for
            // ever —a transparent wall taking every click—. A commit without a
            // new buffer only applies the pending state.
            self.wl.set_input_region(Some(region.wl_region()));
            self.commit();
        }
    }

    /// Takes effect with the next frame presented, like the input region.
    /// Committed now, like the input region: a surface that has just closed
    /// paints no more frames, and a keyboard it was lent stayed lent —what
    /// was typed after closing Marea's calendar kept going to her—.
    fn keyboard(&self, t: Keyboard) {
        if let Some(layer) = &self.layer {
            layer.set_keyboard_interactivity(keyboard_interactivity(t));
            self.commit();
        }
    }

    /// It applies with the `commit` the driver itself does when presenting, like
    /// the input region: the notice arrives when the compositor wants another one.
    fn request_frame(&self) {
        self.wl.frame(&self.qh, FrameFor(self.id));
    }

    fn own_frames(&self) -> Option<Box<dyn gpu::Frames>> {
        (DMABUFS.get().is_some() && !ARGB_LAYOUTS.lock().unwrap().is_empty()).then(|| Box::new(OwnFrames { wl: self.wl.clone(), px: (0, 0), frames: Vec::new(), latest: None, failed: false, to_show: None, shared: self.shown.clone() }) as Box<dyn gpu::Frames>)
    }

    fn desktop_place(&self) -> Option<(String, (i32, i32))> {
        let d = self.backdrop.as_ref()?;
        let (_, at) = d.output_and_position()?;
        let name = d.output.lock().unwrap().as_ref()?.1.clone();
        Some((name, at))
    }

    fn capture_backdrop(&self, bounds: [i32; 4], rects: &[[i32; 4]], on_change: bool) -> bool {
        let (Some(d), Some((manager, _))) = (&self.backdrop, SCREENCOPY.get()) else { return false };
        if d.in_flight.load(Ordering::Relaxed) {
            crate::lens::log_not_asked(d.id, "one is still on the way");
            return false;
        }
        let Some((output, (x, y))) = d.output_and_position() else {
            crate::lens::log_not_asked(d.id, "its monitor or its place on it is not known");
            return false;
        };
        // A monitor that went away (sleep, unplugged) leaves its handle behind, and asking
        // the compositor to photograph it is a fatal protocol error ("invalid output").
        if !output.is_alive() || !OUTPUTS.lock().unwrap().values().any(|o| o == &output) {
            crate::lens::log_not_asked(d.id, "its monitor is gone");
            return false;
        }
        // Not while monitors are coming and going: the one it is on may already be gone over there.
        if OUTPUTS_CHANGED.lock().unwrap().is_some_and(|t| t.elapsed() < OUTPUTS_SETTLE) {
            crate::lens::log_not_asked(d.id, "monitors are coming and going");
            return false;
        }
        if d.in_flight.swap(true, Ordering::Relaxed) {
            return false;
        }
        // Without the cursor: the glass doesn't refract it, it goes on top.
        let mut frames = d.frames.lock().unwrap();
        if rects.len() >= 2 && !PARTS_BROKEN.load(Ordering::Relaxed) {
            let parts = Arc::new(Mutex::new(Parts { hull: bounds, rects: rects.to_vec(), valid: Vec::new(), over: false }));
            for (k, r) in rects.iter().enumerate() {
                frames.push(manager.capture_output_region(0, &output, x + r[0], y + r[1], r[2], r[3], &self.qh, CaptureFor { target: d.clone(), on_change, format: Mutex::new(None), part: Some((parts.clone(), k)) }));
            }
        } else {
            frames.push(manager.capture_output_region(0, &output, x + bounds[0], y + bounds[1], bounds[2], bounds[3], &self.qh, CaptureFor { target: d.clone(), on_change, format: Mutex::new(None), part: None }));
        }
        true
    }

    fn cancel_backdrop(&self) {
        let Some(d) = &self.backdrop else { return };
        for m in d.frames.lock().unwrap().drain(..) {
            m.destroy();
        }
        d.in_flight.store(false, Ordering::Relaxed);
    }

    /// Like the input region, it takes effect with the next frame presented.
    fn update_blur_region(&self, boxes: &[[i32; 4]]) {
        let Some(effects) = BACKGROUND_EFFECTS.get() else { return };
        let mut effect = self.effect.lock().unwrap();
        if effect.is_none() && boxes.is_empty() {
            return;
        }
        let effect = effect.get_or_insert_with(|| effects.get_background_effect(&self.wl, &self.qh, Silent));
        if boxes.is_empty() {
            effect.set_blur_region(None);
        } else if let Ok(region) = Region::new(&self.compositor) {
            for c in boxes {
                region.add(c[0], c[1], c[2] - c[0], c[3] - c[1]);
            }
            effect.set_blur_region(Some(region.wl_region()));
        }
    }

    fn cursor(&self, c: Cursor) {
        let shape = shape_of(c);
        CURSOR_SHAPE.store(c.code(), Ordering::Relaxed);
        if let Some(d) = self.cursors.lock().unwrap().as_ref() {
            d.set_shape(self.serial.load(Ordering::Relaxed), shape);
        }
    }
}

/// The cursor the scene last asked for. A client has to say which cursor it
/// wants every time the pointer ENTERS one of its surfaces, or the compositor
/// draws none: over a transparent full-screen surface that is a mouse that has
/// vanished. So it is kept here and said again on every entry.
static CURSOR_SHAPE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn shape_of(c: Cursor) -> Shape {
    match c {
        Cursor::Normal => Shape::Default,
        Cursor::Hand => Shape::Pointer,
        Cursor::Text => Shape::Text,
        Cursor::Grab => Shape::Grab,
        Cursor::Grabbing => Shape::Grabbing,
        Cursor::EwResize => Shape::EwResize,
        Cursor::NsResize => Shape::NsResize,
        Cursor::NwseResize => Shape::NwseResize,
        Cursor::NeswResize => Shape::NeswResize,
        Cursor::Move => Shape::Move,
        Cursor::NotAllowed => Shape::NotAllowed,
        Cursor::Crosshair => Shape::Crosshair,
        // the protocol numbers its shapes in the order of the names, from 1
        Cursor::Named(i) => Shape::try_from(i as u32 + 1).unwrap_or(Shape::Default),
    }
}

/// How a surface asks for its place: stuck to an edge of the monitor, or as a
/// normal window, which the compositor decorates and places.
#[derive(Clone)]
enum Role {
    Layer(LayerSurface),
    Window(Window),
}

impl Role {
    fn wl(&self) -> &wl_surface::WlSurface {
        match self {
            Role::Layer(c) => c.wl_surface(),
            Role::Window(v) => v.wl_surface(),
        }
    }
    fn layer(&self) -> Option<&LayerSurface> {
        match self {
            Role::Layer(c) => Some(c),
            Role::Window(_) => None,
        }
    }
}

/// A surface placed on a monitor.
struct Placed {
    id: u32,
    /// Which surface of the scene it is.
    which: usize,
    role: Role,
    output: wl_output::WlOutput,
    /// To paint at a scale that isn't whole.
    viewport: Option<WpViewport>,
    _fractional_scale: Option<WpFractionalScaleV1>,
    /// The last scale the compositor said. It usually arrives BEFORE the
    /// surface is configured, when the render doesn't know about it yet.
    scale: f32,
    /// It's handed to the render when the compositor configures it.
    pending: Option<(wgpu::Surface<'static>, String, i32)>,
    /// Its current size: anyone can stretch a window.
    size: (u32, u32),
    /// A layer: which edges it's stuck to and with what margins, to know
    /// where it lands on its monitor; and what's needed to see what's behind.
    layer_placement: Option<(Anchor, [i32; 4])>,
    backdrop: Option<Arc<BackdropTarget>>,
}

struct State {
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    compositor: CompositorState,
    layers: LayerShell,
    viewporter: Option<WpViewporter>,
    fractional_scales: Option<WpFractionalScaleManagerV1>,
    connection: Connection,
    instance: wgpu::Instance,
    wanted: Vec<Surface>,
    extra_height: u32,
    placed: Vec<Placed>,
    pointer: Option<wl_pointer::WlPointer>,
    /// Every pointer it listens to, with its seat: the user's, and an agent's
    /// own (pleamar-wm's `cua-agent`), whose presses on what a scene keeps for
    /// a person's hand (`agent: no`) are not let through.
    pointer_seats: Vec<(wl_pointer::WlPointer, wl_seat::WlSeat)>,
    keyboard: Option<wayland_client::protocol::wl_keyboard::WlKeyboard>,
    cursor_shapes: Option<CursorShapeManager>,
    cursors: Arc<Mutex<Option<WpCursorShapeDeviceV1>>>,
    serial: Arc<AtomicU32>,
    mods: Mods,
    /// To receive what gets dragged from another application.
    drag_and_drop: Option<DataDeviceManagerState>,
    data_device: Option<DataDevice>,
    quit: bool,
    to_render: Sender<ToRender>,
    qh: QueueHandle<State>,
    /// The memory for the captures of what's behind, one per surface.
    /// (and one for each part, when a capture is made of several).
    capture_buffers: std::collections::HashMap<(u32, usize), CaptureBuffer>,
    /// What the parts are copied into, one per surface.
    capture_hulls: std::collections::HashMap<u32, Assembled>,
}

/// The Wayland objects that tell us nothing.
struct Silent;
impl<I: Proxy> Dispatch2<I, State> for Silent {
    fn event(&self, _: &mut State, _: &I, _: I::Event, _: &Connection, _: &QueueHandle<State>) {}
}

/// The compositor has already shown that sheet's last frame and wants another.
/// What isn't seen —a monitor that's off— doesn't get notified: that silence is what
/// lets the render stop painting for nobody.
struct FrameFor(u32);
impl Dispatch2<wayland_client::protocol::wl_callback::WlCallback, State> for FrameFor {
    fn event(&self, e: &mut State, _: &wayland_client::protocol::wl_callback::WlCallback, ev: wayland_client::protocol::wl_callback::Event, _: &Connection, _: &QueueHandle<State>) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = ev {
            let _ = e.to_render.send(ToRender::Frame(self.0));
        }
    }
}

/// The scale the compositor prefers for a surface, in 120ths.
struct ScaleFor(u32);
impl Dispatch2<WpFractionalScaleV1, State> for ScaleFor {
    fn event(&self, e: &mut State, _: &WpFractionalScaleV1, ev: wp_fractional_scale_v1::Event, _: &Connection, _: &QueueHandle<State>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = ev {
            let scale = scale as f32 / 120.0;
            if let Some(p) = e.placed.iter_mut().find(|p| p.id == self.0) {
                p.scale = scale;
            }
            let _ = e.to_render.send(ToRender::Scale(self.0, scale));
        }
    }
}

impl State {
    /// How many surfaces of this kind go on this monitor. `k` is the monitor's
    /// number, in order of appearance: it's what `screens: each` hands out.
    /// The wgpu surface that paints on a Wayland one.
    fn wgpu_surface_for(&self, wl: &wl_surface::WlSurface) -> wgpu::Surface<'static> {
        unsafe {
            self.instance
                .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                    raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                        NonNull::new(self.connection.backend().display_ptr() as *mut _).unwrap(),
                    ))),
                    raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(NonNull::new(wl.id().as_ptr() as *mut _).unwrap())),
                })
                .expect("the graphics surface could not be created: no Vulkan or OpenGL driver took it (install your card's Vulkan driver and the Vulkan loader, vulkan-icd-loader)")
        }
    }

    fn copies_wanted_on(s: &Surface, name: &str, k: usize) -> usize {
        match &s.screens {
            Screens::All => 1,
            Screens::Named(n) => n.iter().filter(|x| x.as_str() == name).count(),
            Screens::Number(x) => (*x == k) as usize,
        }
    }

    /// Puts on a monitor the surfaces the scene asks for it.
    fn place_on(&mut self, output: &wl_output::WlOutput, qh: &QueueHandle<State>) {
        let Some(info) = self.outputs.info(output) else { return };
        let name = info.name.clone().unwrap_or_default();
        OUTPUTS.lock().unwrap().insert(name.clone(), output.clone());
        // Which monitor number it is: the order in which the compositor counts them.
        let number = self.outputs.outputs().position(|o| &o == output).unwrap_or(0);
        let mhz = info.modes.iter().find(|m| m.current).map_or(0, |m| m.refresh_rate);
        if let Some(c) = LOCKS.get() {
            let mut m = c.monitors.lock().unwrap();
            if !m.iter().any(|(s, _, _)| s == output) {
                m.push((output.clone(), name.clone(), mhz));
            }
        }
        lock_face_for_new_monitor(output, &name, number, mhz);
        self.drop_moved_copies(output, number);
        for which in 0..self.wanted.len() {
            // The lock one isn't placed: it's engaged, when the scene says so.
            if self.wanted[which].lock_screen {
                continue;
            }
            let already = self.placed.iter().filter(|p| &p.output == output && p.which == which).count();
            let wants = Self::copies_wanted_on(&self.wanted[which], &name, number);
            for k in already..wants {
            let p = &self.wanted[which];
            // The HUD only goes below the main one.
            let height = p.height + if which == 0 { self.extra_height } else { 0 };
            // A normal window isn't placed per monitor: the compositor places it.
            if p.window.is_some() && self.placed.iter().any(|x| x.which == which) {
                continue;
            }
            let wl = self.compositor.create_surface(qh);
            let level = layer_of(p.level);
            // `kind: window`: one of the normal windows, with its title and its frame.
            if let (Some(title), Some(pp)) = (&p.window, POPUPS.get()) {
                let window = pp.xdg.create_window(wl, WindowDecorations::RequestServer, qh);
                window.set_title(if title.is_empty() { "pleamar" } else { title });
                window.set_app_id(p.app_id.as_deref().unwrap_or("pleamar"));
                window.set_min_size(Some((p.width, height)));
                let id = next_surface_id();
                window.commit();
                let role = Role::Window(window);
                let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(role.wl(), qh, Silent));
                let fractional_scale = self.fractional_scales.as_ref().map(|m| m.get_fractional_scale(role.wl(), qh, ScaleFor(id)));
                let surface = self.wgpu_surface_for(role.wl());
                let output_name = self.outputs.info(output).and_then(|i| i.name).unwrap_or_default();
                let backdrop = Some(BackdropTarget::new(id, Some((output.clone(), output_name)), BackdropKind::Window));
                self.placed.push(Placed { id, which, role, output: output.clone(), viewport, _fractional_scale: fractional_scale, scale: 1.0, size: (0, 0), pending: Some((surface, name.clone(), mhz)), layer_placement: None, backdrop });
                continue;
            }
            let layer = self.layers.create_layer_surface(qh, wl, level, Some("pleamar"), Some(output));
            let reserves = p.exclusive_zone > 0 || p.reserve_while.is_some();
            let mut stretch = Stretch { width: p.width == 0, height: p.height == 0, monitor: if reserves { info.logical_size } else { None }, edges: Anchor::empty(), screen: info.logical_size, extra_height: height - p.height };
            let (edges, size) = placement(p.anchor, stretch, (p.width, height));
            stretch.edges = edges;
            layer.set_anchor(edges);
            // The second one on the same monitor, below the first: it's for trying things out.
            let m = p.margin;
            let margin = [m[0] + k as i32 * (height as i32 + 12), m[1], m[2], m[3]];
            layer.set_margin(margin[0], margin[1], margin[2], margin[3]);
            // Noted down, in case the scene moves it to another edge while running,
            // or a reload gives it another size, edge, margin, room or level.
            if let Some(c) = MOVABLE_LAYERS.get() {
                c.placed.lock().unwrap().push((which, layer.clone(), margin, stretch));
            }
            layer.set_size(size.0, size.1);
            // With a condition, none until the render says it holds.
            layer.set_exclusive_zone(if p.reserve_while.is_some() { 0 } else { p.exclusive_zone });
            // If the keyboard depends on something (`exclusive while open`), it's born without it.
            layer.set_keyboard_interactivity(keyboard_interactivity(if p.keyboard_while { Keyboard::Never } else { p.keyboard }));
            let id = next_surface_id();
            // With a viewport, the logical size is fixed and the real pixels are
            // decided by the scale: that way a scale that isn't whole also works.
            // Width 0 is "the whole monitor": how much that is, the compositor will say when configuring it.
            let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(layer.wl_surface(), qh, Silent));
            let fractional_scale = self.fractional_scales.as_ref().map(|m| m.get_fractional_scale(layer.wl_surface(), qh, ScaleFor(id)));
            layer.commit();
            let surface = self.wgpu_surface_for(layer.wl_surface());
            let output_name = self.outputs.info(output).and_then(|i| i.name).unwrap_or_default();
            let backdrop = Some(BackdropTarget::new(id, Some((output.clone(), output_name)), BackdropKind::Layer));
            let layer_placement = Some((edges, margin));
            self.placed.push(Placed { id, which, role: Role::Layer(layer), output: output.clone(), viewport, _fractional_scale: fractional_scale, scale: 1.0, size: (0, 0), pending: Some((surface, name.clone(), mhz)), layer_placement, backdrop });
            LAYERS_PLACED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// A copy handed out by number (`screens: each`, copy k on monitor k) that this monitor's number no longer gives it goes away. The monitors
    /// are numbered in the order the compositor lists them, and that order changes when one comes or goes: one switched on after the session
    /// started, or woken from standby, is listed again, last, and the others move up. Each was given the copy of its new number, and kept the
    /// one of its old number too: the same copy on two monitors, so the bar and the desktop widgets showed twice and the panels opened on the
    /// wrong one, until the program was started again. The copy itself (its springs, its facts) lives on in the render, and shows again where
    /// its number now is.
    fn drop_moved_copies(&mut self, output: &wl_output::WlOutput, number: usize) {
        let moved = |p: &Placed, wanted: &[Surface]| {
            &p.output == output
                && matches!(p.role, Role::Layer(_))
                && wanted.get(p.which).is_some_and(|s| matches!(s.screens, Screens::Number(x) if x != number))
        };
        let wanted = &self.wanted;
        for p in self.placed.iter().filter(|p| moved(p, wanted)) {
            let _ = self.to_render.send(ToRender::SheetGone(p.id));
            if let Some(c) = MOVABLE_LAYERS.get() {
                c.placed.lock().unwrap().retain(|(_, layer, _, _)| layer.wl_surface() != p.role.wl());
            }
        }
        self.placed.retain(|p| !moved(p, wanted));
    }

    /// Every monitor gets again what its number gives it: when one comes or goes the others' numbers change, and the compositor need not say
    /// anything else about them.
    fn place_on_all(&mut self, qh: &QueueHandle<State>, except: Option<&wl_output::WlOutput>) {
        let all: Vec<wl_output::WlOutput> = self.outputs.outputs().filter(|o| Some(o) != except).collect();
        for o in &all {
            self.place_on(o, qh);
        }
    }

    fn remove_from(&mut self, output: &wl_output::WlOutput) {
        OUTPUTS.lock().unwrap().retain(|_, o| o != output);
        if let Some(c) = LOCKS.get() {
            c.monitors.lock().unwrap().retain(|(s, _, _)| s != output);
        }
        for id in lock_face_for_gone_monitor(output) {
            let _ = self.to_render.send(ToRender::SheetGone(id));
        }
        for p in self.placed.iter().filter(|p| &p.output == output) {
            let _ = self.to_render.send(ToRender::SheetGone(p.id));
            if let Some(c) = MOVABLE_LAYERS.get() {
                c.placed.lock().unwrap().retain(|(_, layer, _, _)| layer.wl_surface() != p.role.wl());
            }
        }
        self.placed.retain(|p| &p.output != output);
    }
}

// ── popups ────────────────────────────────────────────────────────

/// What's needed to open a popup from the render thread, which is the one
/// that knows when it's time. Wayland objects can be used from any
/// thread; what happens to them afterwards arrives on the usual one, through `PopupHandler`.
struct Popups {
    qh: QueueHandle<State>,
    compositor: CompositorState,
    xdg: XdgShell,
    connection: Connection,
    instance: wgpu::Instance,
    viewporter: Option<WpViewporter>,
    fractional_scales: Option<WpFractionalScaleManagerV1>,
    cursors: Arc<Mutex<Option<WpCursorShapeDeviceV1>>>,
    serial: Arc<AtomicU32>,
    /// What the next one hangs from: the surface where the mouse was last seen.
    parent: Mutex<Option<Parent>>,
    seat: Mutex<Option<wl_seat::WlSeat>>,
    /// The last press: with it one can ask for a click outside to close it.
    last_press: Mutex<Option<(u32, std::time::Instant)>>,
    open: Mutex<Vec<OpenPopup>>,
}

#[derive(Clone)]
struct Parent {
    layer: LayerSurface,
    /// Where it is: a popup is where its parent is, plus whatever the compositor says.
    backdrop: Option<Arc<BackdropTarget>>,
    scale: f32,
    name: String,
    mhz: i32,
}

struct OpenPopup {
    k: usize,
    id: u32,
    backdrop: Option<Arc<BackdropTarget>>,
    origin: (f32, f32),
    size: (u32, u32),
    // In order: first what paints is released, then the surface.
    pending: Option<wgpu::Surface<'static>>,
    viewport: Option<WpViewport>,
    _fractional_scale: Option<WpFractionalScaleV1>,
    parent: Parent,
    popup: Popup,
}

// ── the lock ──────────────────────────────────────────────────────

/// The lock screen (`kind: lock`). It's not a surface painted
/// on top of everything: it's the `ext-session-lock` protocol, with which the COMPOSITOR
/// guarantees that while it lasts nothing else is seen or touched, on any
/// monitor. That's why it doesn't exist until it's engaged, and that's why the render opens it
/// —it's the one that knows when the scene's `open:` becomes true— just like
/// a popup. What happens afterwards arrives on the usual thread.
///
/// And one thing you need to know before using it: if whoever locks dies with
/// the lock engaged, the session STAYS locked. It's on purpose —the opposite
/// would be that killing the locker unlocked it— and it's what forces this to
/// never fail halfway.
struct Locks {
    qh: QueueHandle<State>,
    compositor: CompositorState,
    manager: smithay_client_toolkit::session_lock::SessionLockState,
    connection: Connection,
    instance: wgpu::Instance,
    viewporter: Option<WpViewporter>,
    fractional_scales: Option<WpFractionalScaleManagerV1>,
    /// The monitors there are now, with their name and their refresh rate: the
    /// Wayland thread keeps them up to date, and one is needed per face.
    monitors: Mutex<Vec<(wl_output::WlOutput, String, i32)>>,
    engaged: Mutex<Option<Engaged>>,
    /// For the plain covers of the monitors the scene is not on.
    shm: Option<WlShm>,
}

struct Engaged {
    which: usize,
    /// The box the scene declares (`size:`), which gets centred on each monitor,
    /// and where its piece of the plane lands.
    bounds: (u32, u32),
    origin: (f32, f32),
    /// Which monitors show the scene (`screens:`): a monitor that arrives
    /// while it is locked gets its face by the same rule.
    screens: Screens,
    lock: smithay_client_toolkit::session_lock::SessionLock,
    faces: Vec<LockFace>,
    /// Faces whose monitor has gone but that the render still paints: they are
    /// destroyed only once it has let go of them (`sheet_released`). Destroying
    /// the surface under a swapchain that still uses it is a protocol error, and
    /// the compositor ends the locker for it.
    retiring: Vec<LockFace>,
}

/// The lock surface of ONE monitor.
struct LockFace {
    id: u32,
    // In order: first what paints is released, then the surface.
    pending: Option<wgpu::Surface<'static>>,
    viewport: Option<WpViewport>,
    _fractional_scale: Option<WpFractionalScaleV1>,
    name: String,
    mhz: i32,
    /// Where it looks on the plane: the scene's origin minus whatever is needed
    /// for its box to end up centred. It's known when configuring it.
    view_origin: (f32, f32),
    /// A monitor the scene is not on (`screens:`): it gets a black cover, and
    /// this is its buffer once configured.
    cover: Option<Option<(WlShmPool, WlBuffer, std::os::fd::OwnedFd, (u32, u32))>>,
    /// The monitor it covers: when that one goes, so does the face.
    output: wl_output::WlOutput,
    surface: smithay_client_toolkit::session_lock::SessionLockSurface,
}

static LOCKS: std::sync::OnceLock<Locks> = std::sync::OnceLock::new();

/// `Some((bounds, origin, screens))` engages the lock of surface `which`; `None`
/// releases it. The render calls it, having already released what it painted before releasing it.
pub fn lock_screen(which: usize, what: Option<((u32, u32), (f32, f32), Screens)>) {
    let Some(c) = LOCKS.get() else { return };
    let mut engaged = c.engaged.lock().unwrap();
    let Some((bounds, origin, screens)) = what else {
        if let Some(e) = engaged.take() {
            e.lock.unlock();
            drop(e);
            let _ = c.connection.flush();
            println!("lock   · unlocked");
        }
        return;
    };
    if engaged.is_some() {
        return;
    }
    let lock = match c.manager.lock(&c.qh) {
        Ok(l) => l,
        Err(_) => {
            eprintln!("lock   · this compositor has no ext-session-lock: the session cannot be locked from here");
            return;
        }
    };
    // One face per monitor, and all at once: until they're all there, the
    // compositor doesn't consider the session locked. The monitors the scene is
    // not on (`screens:`) are covered all the same, in black.
    let monitors = c.monitors.lock().unwrap();
    let shown: Vec<bool> = monitors
        .iter()
        .enumerate()
        .map(|(number, (_, name, _))| lock_shows(&screens, name, number))
        .collect();
    // On none of them (a monitor unplugged), on all: a lock with nowhere to type
    // the password would be no way back.
    let everywhere = !shown.contains(&true);
    let faces = monitors
        .iter()
        .zip(shown)
        .map(|((output, name, mhz), shows)| lock_face(c, &lock, output, name, *mhz, shows || everywhere, origin))
        .collect();
    drop(monitors);
    *engaged = Some(Engaged { which, bounds, origin, screens, lock, faces, retiring: Vec::new() });
    let _ = c.connection.flush();
}

/// The lock face of one monitor: the scene, if `shows`, or else a plain black
/// cover, far from anything of the scene so that the mouse over it touches nothing.
fn lock_face(c: &Locks, lock: &smithay_client_toolkit::session_lock::SessionLock, output: &wl_output::WlOutput, name: &str, mhz: i32, shows: bool, origin: (f32, f32)) -> LockFace {
    let wl = c.compositor.create_surface(&c.qh);
    let id = next_surface_id();
    let surface = lock.create_lock_surface(wl, output, &c.qh);
    if !shows && c.shm.is_some() {
        return LockFace { id, pending: None, viewport: None, _fractional_scale: None, name: name.to_string(), mhz, view_origin: (-1e7, -1e7), cover: Some(None), output: output.clone(), surface };
    }
    let viewport = c.viewporter.as_ref().map(|v| v.get_viewport(surface.wl_surface(), &c.qh, Silent));
    let fractional_scale = c.fractional_scales.as_ref().map(|m| m.get_fractional_scale(surface.wl_surface(), &c.qh, ScaleFor(id)));
    let paints = unsafe {
        c.instance
            .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(NonNull::new(c.connection.backend().display_ptr() as *mut _).unwrap()))),
                raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(NonNull::new(surface.wl_surface().id().as_ptr() as *mut _).unwrap())),
            })
            .ok()
    };
    LockFace { id, pending: paints, viewport, _fractional_scale: fractional_scale, name: name.to_string(), mhz, view_origin: origin, cover: None, output: output.clone(), surface }
}

/// Which monitors show the scene, by the lock's `screens:`.
fn lock_shows(screens: &Screens, name: &str, number: usize) -> bool {
    match screens {
        Screens::All => true,
        Screens::Named(n) => n.iter().any(|x| x == name),
        Screens::Number(x) => *x == number,
    }
}

/// A monitor that arrives while the session is locked —one that slept so deep
/// it was unplugged, and woke up— gets a face of its own. Without it the
/// compositor shows its own fallback there: still locked, but not our lock.
///
/// It shows the scene only if `screens:` names it. When none of the faces shows
/// the scene (the monitors of `screens:` are still away: on waking, the
/// compositor's stand-in FALLBACK and the first real monitor come before the
/// one the lock is meant for), the black cover waits `LOCK_GRACE` and then, if
/// still nothing shows it, becomes the scene: a lock with nowhere to type the
/// password would be no way back. Waiting is what keeps the lock off the
/// monitors that were not chosen: a face that paints is only let go of by the
/// render, so one that showed the scene at once would stay on the wrong monitor.
fn lock_face_for_new_monitor(output: &wl_output::WlOutput, name: &str, number: usize, mhz: i32) {
    let Some(c) = LOCKS.get() else { return };
    let mut engaged = c.engaged.lock().unwrap();
    let Some(e) = engaged.as_mut() else { return };
    if e.faces.iter().any(|f| &f.output == output) {
        return;
    }
    let shows = lock_shows(&e.screens, name, number);
    let face = lock_face(c, &e.lock, output, name, mhz, shows, e.origin);
    let covered = face.cover.is_some();
    e.faces.push(face);
    let _ = c.connection.flush();
    println!("lock   · {name} arrived while locked: it has its face");
    if covered && e.faces.iter().all(|f| f.cover.is_some()) {
        std::thread::spawn(|| {
            std::thread::sleep(LOCK_GRACE);
            let Some(c) = LOCKS.get() else { return };
            let mut engaged = c.engaged.lock().unwrap();
            if let Some(e) = engaged.as_mut() {
                promote_first_cover(c, e);
            }
        });
    }
}

/// How long the monitors of `screens:` are waited for before a cover shows the scene.
const LOCK_GRACE: std::time::Duration = std::time::Duration::from_secs(4);

/// If what there is are only black covers, the first of them becomes the scene.
/// A cover paints nothing, so it can be destroyed here, and it is destroyed
/// before the new face is made, since a monitor can only have one.
fn promote_first_cover(c: &Locks, e: &mut Engaged) {
    if e.faces.is_empty() || !e.faces.iter().all(|f| f.cover.is_some()) {
        return;
    }
    let cover = e.faces.remove(0);
    let (output, name, mhz) = (cover.output.clone(), cover.name.clone(), cover.mhz);
    drop(cover);
    let face = lock_face(c, &e.lock, &output, &name, mhz, true, e.origin);
    e.faces.insert(0, face);
    let _ = c.connection.flush();
    println!("lock   · {name} shows the lock: the monitors it was meant for are not here");
}

/// A monitor that goes while the session is locked takes its face with it. A
/// face the render paints waits in `retiring` until the render has let go of it,
/// as on unlock; a cover, or a face not yet configured, is destroyed here. If
/// what is left are only black covers, the first of them becomes the scene: a
/// cover paints nothing, so it can be destroyed here, and it is destroyed before
/// the new face is made, since a monitor can only have one. Gives back the ids
/// the render has to let go of.
fn lock_face_for_gone_monitor(output: &wl_output::WlOutput) -> Vec<u32> {
    let Some(c) = LOCKS.get() else { return Vec::new() };
    let mut engaged = c.engaged.lock().unwrap();
    let Some(e) = engaged.as_mut() else { return Vec::new() };
    let mut gone = Vec::new();
    let (going, staying): (Vec<LockFace>, Vec<LockFace>) = std::mem::take(&mut e.faces).into_iter().partition(|f| &f.output == output);
    e.faces = staying;
    for f in going {
        if f.cover.is_none() && f.pending.is_none() {
            gone.push(f.id);
            e.retiring.push(f);
        }
    }
    promote_first_cover(c, e);
    let _ = c.connection.flush();
    gone
}

/// The render has let go of surface `id`: if it was the face of a monitor that
/// went while locked, it can be destroyed now.
pub fn sheet_released(id: u32) {
    let Some(c) = LOCKS.get() else { return };
    let mut engaged = c.engaged.lock().unwrap();
    let Some(e) = engaged.as_mut() else { return };
    let before = e.retiring.len();
    e.retiring.retain(|f| f.id != id);
    if e.retiring.len() != before {
        let _ = c.connection.flush();
    }
}

/// The edges it sticks to, as protocol flags. With width 0 it also
/// sticks to left and right: that's what stretches it from side to side; with
/// height 0, to the top and the bottom.
fn anchor_edges(anchor: SurfaceAnchor, full_width: bool, full_height: bool) -> Anchor {
    (match anchor {
        SurfaceAnchor::Top => Anchor::TOP,
        SurfaceAnchor::Bottom => Anchor::BOTTOM,
        SurfaceAnchor::Left => Anchor::LEFT,
        SurfaceAnchor::Right => Anchor::RIGHT,
        SurfaceAnchor::TopLeft => Anchor::TOP | Anchor::LEFT,
        SurfaceAnchor::TopRight => Anchor::TOP | Anchor::RIGHT,
        SurfaceAnchor::BottomLeft => Anchor::BOTTOM | Anchor::LEFT,
        SurfaceAnchor::BottomRight => Anchor::BOTTOM | Anchor::RIGHT,
        SurfaceAnchor::Center => Anchor::empty(),
    }) | if full_width { Anchor::LEFT | Anchor::RIGHT } else { Anchor::empty() }
        | if full_height { Anchor::TOP | Anchor::BOTTOM } else { Anchor::empty() }
}

/// How a surface stretches: the whole width, the whole height of its monitor,
/// and —for one that is both and keeps a reserve— the monitor's logical size.
#[derive(Clone, Copy)]
struct Stretch {
    width: bool,
    height: bool,
    monitor: Option<(i32, i32)>,
    /// The edges it holds now, to know along which one it asked for the monitor's size when the monitor changes (`update_output`).
    edges: Anchor,
    /// Its monitor's size even when it keeps no room, and what the HUD adds
    /// below the main one: for `reshape`, when a reload asks it to keep some.
    screen: Option<(i32, i32)>,
    extra_height: u32,
}

/// The edges it sticks to, and the size to ask for. A surface as big as the
/// monitor that keeps a reserve (`size: full, full; anchor: left; reserve: 68`)
/// cannot stick to all four edges, where layer-shell ignores any reserve: it
/// sticks to its own edge and the two across it, and asks for the monitor's
/// size along the fourth.
fn placement(anchor: SurfaceAnchor, s: Stretch, asked: (u32, u32)) -> (Anchor, (u32, u32)) {
    if let (true, true, Some((w, h))) = (s.width, s.height, s.monitor) {
        let (w, h) = (w.max(0) as u32, h.max(0) as u32);
        match anchor {
            SurfaceAnchor::Left => return (Anchor::LEFT | Anchor::TOP | Anchor::BOTTOM, (w, 0)),
            SurfaceAnchor::Right => return (Anchor::RIGHT | Anchor::TOP | Anchor::BOTTOM, (w, 0)),
            SurfaceAnchor::Top => return (Anchor::TOP | Anchor::LEFT | Anchor::RIGHT, (0, h)),
            SurfaceAnchor::Bottom => return (Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT, (0, h)),
            _ => {}
        }
    }
    (anchor_edges(anchor, s.width, s.height), asked)
}

/// The layers that can change edge, to reach them without going through the
/// Wayland thread. `set_anchor` and `set_margin` are requests and work while
/// running: there's no need to create the surface again, which is what left
/// Marea unable to choose a corner while recording.
struct MovableLayers {
    connection: Connection,
    placed: Mutex<Vec<(usize, LayerSurface, [i32; 4], Stretch)>>,
}
static MOVABLE_LAYERS: std::sync::OnceLock<MovableLayers> = std::sync::OnceLock::new();
static LAYERS_PLACED: AtomicU64 = AtomicU64::new(0);

/// How many layers have been put on a monitor so far (see `platform::layers_placed`).
pub fn layers_placed() -> u64 {
    LAYERS_PLACED.load(Ordering::Relaxed)
}

fn layer_of(level: Level) -> Layer {
    match level {
        Level::Background => Layer::Background,
        Level::Below => Layer::Bottom,
        Level::Above => Layer::Top,
        Level::Overlay => Layer::Overlay,
    }
}

pub fn relayer(which: usize, level: Level) {
    let Some(c) = MOVABLE_LAYERS.get() else { return };
    let mut any = false;
    for (k, layer, _, _) in c.placed.lock().unwrap().iter() {
        if *k == which {
            layer.set_layer(layer_of(level));
            layer.commit();
            any = true;
        }
    }
    if any {
        let _ = c.connection.flush();
    }
}

pub fn rezone(which: usize, zone: i32) {
    let Some(c) = MOVABLE_LAYERS.get() else { return };
    let mut any = false;
    for (k, layer, _, _) in c.placed.lock().unwrap().iter() {
        if *k == which {
            layer.set_exclusive_zone(zone);
            layer.commit();
            any = true;
        }
    }
    if any {
        let _ = c.connection.flush();
    }
}

pub fn reanchor(which: usize, anchor: SurfaceAnchor) {
    let Some(c) = MOVABLE_LAYERS.get() else { return };
    let mut any = false;
    for (k, layer, margin, stretch) in c.placed.lock().unwrap().iter_mut() {
        if *k != which {
            continue;
        }
        let (edges, size) = placement(anchor, *stretch, (0, 0));
        stretch.edges = edges;
        layer.set_anchor(edges);
        // As big as the monitor: from one edge to another, the size along the fourth changes too.
        if stretch.width && stretch.height {
            layer.set_size(size.0, size.1);
        }
        layer.set_margin(margin[0], margin[1], margin[2], margin[3]);
        layer.commit();
        any = true;
    }
    if any {
        let _ = c.connection.flush();
    }
}

/// A surface its scene was saved with at another size, edge, margin, room or
/// level: changed on the live layer, as layer-shell allows, without creating
/// it again. The compositor answers with the new size, which the surface takes
/// as it takes any other.
pub fn reshape(which: usize, p: &crate::scene::Surface) {
    let Some(c) = MOVABLE_LAYERS.get() else { return };
    let mut any = false;
    for (k, layer, margin, stretch) in c.placed.lock().unwrap().iter_mut() {
        if *k != which {
            continue;
        }
        let reserves = p.exclusive_zone > 0 || p.reserve_while.is_some();
        stretch.width = p.width == 0;
        stretch.height = p.height == 0;
        stretch.monitor = if reserves { stretch.screen } else { None };
        let (edges, size) = placement(p.anchor, *stretch, (p.width, p.height + stretch.extra_height));
        stretch.edges = edges;
        layer.set_anchor(edges);
        *margin = p.margin;
        layer.set_margin(margin[0], margin[1], margin[2], margin[3]);
        layer.set_size(size.0, size.1);
        // With a condition, the render sets its room as it holds.
        if p.reserve_while.is_none() {
            layer.set_exclusive_zone(p.exclusive_zone);
        }
        layer.set_layer(layer_of(p.level));
        layer.commit();
        any = true;
    }
    if any {
        let _ = c.connection.flush();
    }
}

static POPUPS: std::sync::OnceLock<Popups> = std::sync::OnceLock::new();

pub fn popup(k: usize, what: Option<([i32; 4], (f32, f32))>) {
    let Some(e) = POPUPS.get() else { return };
    let Some(([x, y, w, h], origin)) = what else {
        e.open.lock().unwrap().retain(|a| a.k != k);
        let _ = e.connection.flush();
        return;
    };
    let Some(parent) = e.parent.lock().unwrap().clone() else { return };
    let open = || -> Option<OpenPopup> {
        let positioner = XdgPositioner::new(&e.xdg).ok()?;
        positioner.set_size(w, h);
        positioner.set_anchor_rect(x, y, 1, 1);
        {
            use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_positioner::{Anchor, ConstraintAdjustment, Gravity};
            positioner.set_anchor(Anchor::TopLeft);
            positioner.set_gravity(Gravity::BottomRight);
            // If it doesn't fit on the screen, let it slide until it fits.
            positioner.set_constraint_adjustment(ConstraintAdjustment::SlideX | ConstraintAdjustment::SlideY);
        }
        let wl = e.compositor.create_surface(&e.qh);
        let popup = Popup::from_surface(None, &positioner, &e.qh, wl, &e.xdg).ok()?;
        parent.layer.get_popup(popup.xdg_popup());
        // If it opens because of a click a moment ago, let a click outside close it.
        if let (Some(seat), Some((serial, when))) = (e.seat.lock().unwrap().as_ref(), *e.last_press.lock().unwrap()) {
            if when.elapsed() < std::time::Duration::from_millis(1500) {
                popup.xdg_popup().grab(seat, serial);
            }
        }
        let id = next_surface_id();
        let viewport = e.viewporter.as_ref().map(|v| v.get_viewport(popup.wl_surface(), &e.qh, Silent));
        let fractional_scale = e.fractional_scales.as_ref().map(|m| m.get_fractional_scale(popup.wl_surface(), &e.qh, ScaleFor(id)));
        popup.wl_surface().commit();
        let surface = unsafe {
            e.instance
                .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                    raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(NonNull::new(e.connection.backend().display_ptr() as *mut _).unwrap()))),
                    raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(NonNull::new(popup.wl_surface().id().as_ptr() as *mut _).unwrap())),
                })
                .ok()?
        };
        let backdrop = parent.backdrop.clone().map(|m| {
            let d = BackdropTarget::new(id, None, BackdropKind::Popup { parent: m, offset: Mutex::new((0, 0)) });
            *d.size.lock().unwrap() = (w as u32, h as u32);
            d
        });
        Some(OpenPopup { k, id, backdrop, origin, size: (w as u32, h as u32), pending: Some(surface), viewport, _fractional_scale: fractional_scale, parent: parent.clone(), popup })
    };
    match open() {
        Some(a) => e.open.lock().unwrap().push(a),
        None => eprintln!("popup  · the compositor did not let it open"),
    }
    let _ = e.connection.flush();
}

/// We don't open normal windows yet (it's what's left of S7), but the
/// popup protocol comes in the same package and requires this to exist.
impl WindowHandler for State {
    /// Someone clicked the close button. That one closes; if it was the last one, it's over.
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, v: &Window) {
        self.surface_gone(&v.wl_surface().clone());
        if self.placed.is_empty() {
            self.quit = true;
        }
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, v: &Window, conf: WindowConfigure, _: u32) {
        let size = (conf.new_size.0.map_or(0, |x| x.get()), conf.new_size.1.map_or(0, |x| x.get()));
        self.configured(&v.wl_surface().clone(), size);
    }
}

impl PopupHandler for State {
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, popup: &Popup, conf: PopupConfigure) {
        let Some(e) = POPUPS.get() else { return };
        let mut open = e.open.lock().unwrap();
        let Some(a) = open.iter_mut().find(|a| a.popup.wl_surface() == popup.wl_surface()) else { return };
        // Where the compositor has put it relative to its parent: it may have
        // flipped it so that it fits.
        if let Some(BackdropKind::Popup { offset, .. }) = a.backdrop.as_ref().map(|d| &d.kind) {
            *offset.lock().unwrap() = conf.position;
        }
        if let Some(v) = &a.viewport {
            v.set_destination(a.size.0 as i32, a.size.1 as i32);
        }
        if let Some(surface) = a.pending.take() {
            let _ = self.to_render.send(ToRender::Sheet(Box::new(gpu::NewSheet {
                id: a.id,
                target: gpu::Target::Surface(surface),
                window: Box::new(WaylandWindow { wl: a.popup.wl_surface().clone(), compositor: self.compositor.clone(), cursors: e.cursors.clone(), serial: e.serial.clone(), layer: None, qh: e.qh.clone(), id: a.id, effect: Mutex::new(None), backdrop: a.backdrop.clone(), shown: Default::default() }),
                scale: a.parent.scale,
                size: a.size,
                mhz: a.parent.mhz,
                name: format!("{} (popup)", a.parent.name),
                view: gpu::View { surface: 0, popup: Some(a.k), origin: a.origin, size: (a.size.0 as f32, a.size.1 as f32) },
            })));
        }
    }

    /// Someone clicked outside, or the compositor removed it. The render releases its stuff and closes it.
    fn done(&mut self, _: &Connection, _: &QueueHandle<Self>, popup: &Popup) {
        let Some(e) = POPUPS.get() else { return };
        if let Some(a) = e.open.lock().unwrap().iter().find(|a| a.popup.wl_surface() == popup.wl_surface()) {
            let _ = self.to_render.send(ToRender::PopupClosed(a.k));
        }
    }
}

/// Puts up the surfaces the scene asks for and attends to Wayland until
/// someone closes. It keeps the thread that calls it.
/// The session's connection, made once: the one the surfaces are put up on, and
/// the display OpenGL has to know before it can paint on them (EGL makes its
/// display from it; one made from another connection does not take its surfaces).
pub fn session_connection() -> Option<Connection> {
    static C: std::sync::OnceLock<Option<Connection>> = std::sync::OnceLock::new();
    C.get_or_init(|| Connection::connect_to_env().ok()).clone()
}

/// That connection's display, as a graphics instance takes it.
pub struct SessionDisplay(Connection);
impl std::fmt::Debug for SessionDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionDisplay")
    }
}
impl raw_window_handle::HasDisplayHandle for SessionDisplay {
    fn display_handle(&self) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        let ptr = NonNull::new(self.0.backend().display_ptr() as *mut _).ok_or(raw_window_handle::HandleError::Unavailable)?;
        Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(ptr))) })
    }
}
pub fn session_display() -> Option<SessionDisplay> {
    session_connection().map(SessionDisplay)
}

pub fn run_event_loop(wanted: Vec<Surface>, extra_height: u32, instance: wgpu::Instance, to_render: Sender<ToRender>) {
    let connection = session_connection().expect("there is no Wayland session");
    // Which display, beside its socket: `--say` prefers a scene on the caller's.
    if let Some(d) = super::display_here() {
        super::shown_on(d);
    }
    let (globals, mut events) = registry_queue_init::<State>(&connection).unwrap();
    let qh = events.handle();
    let compositor = CompositorState::bind(&globals, &qh).expect("no wl_compositor");
    if let Ok(m) = DataDeviceManagerState::bind(&globals, &qh) {
        let _ = DRAGS.set(Drags { connection: connection.clone(), qh: qh.clone(), manager: m.data_device_manager().clone(), device: Mutex::default(), press: Mutex::default(), under_way: Mutex::default(), compositor: CompositorState::bind(&globals, &qh).expect("no wl_compositor"), shm: globals.bind::<WlShm, _, _>(&qh, 1..=1, Silent).ok(), icon: Mutex::default() });
    }
    if let Ok(m) = globals.bind::<ExtBackgroundEffectManagerV1, _, _>(&qh, 1..=1, Silent) {
        let _ = BACKGROUND_EFFECTS.set(m);
    }
    if let Ok(global) = globals.bind::<ZwpLinuxDmabufV1, _, _>(&qh, 3..=3, Layouts) {
        let _ = DMABUFS.set(Dmabufs { global, qh: qh.clone(), connection: connection.clone() });
    }
    // Bound a second time, at 4, only for which card it paints with: there it
    // no longer says its layouts one by one, as the other one needs. The
    // answer comes with the round trips below, before the first sheet's card is chosen.
    if let Ok(v4) = globals.bind::<ZwpLinuxDmabufV1, _, _>(&qh, 4..=4, Silent) {
        v4.get_default_feedback(&qh, MainDevice);
    }
    if let (Ok(c), Ok(s)) = (globals.bind::<ZwlrScreencopyManagerV1, _, _>(&qh, 1..=3, Silent), globals.bind::<WlShm, _, _>(&qh, 1..=1, Silent)) {
        let _ = SCREENCOPY.set((c, s));
    }
    let mut state = State {
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        outputs: OutputState::new(&globals, &qh),
        layers: LayerShell::bind(&globals, &qh).expect("the compositor has no layer-shell"),
        viewporter: globals.bind(&qh, 1..=1, Silent).ok(),
        fractional_scales: globals.bind(&qh, 1..=1, Silent).ok(),
        compositor,
        connection: connection.clone(),
        qh: qh.clone(),
        capture_buffers: std::collections::HashMap::new(),
        capture_hulls: std::collections::HashMap::new(),
        instance,
        wanted,
        extra_height,
        placed: Vec::new(),
        pointer: None,
        pointer_seats: Vec::new(),
        keyboard: None,
        cursor_shapes: CursorShapeManager::bind(&globals, &qh).ok(),
        cursors: Arc::default(),
        serial: Arc::default(),
        mods: Mods::default(),
        drag_and_drop: DataDeviceManagerState::bind(&globals, &qh).ok(),
        data_device: None,
        quit: false,
        to_render,
    };
    match XdgShell::bind(&globals, &qh) {
        Ok(xdg) => {
            let _ = MOVABLE_LAYERS.set(MovableLayers { connection: connection.clone(), placed: Mutex::default() });
            let _ = POPUPS.set(Popups {
                qh: qh.clone(),
                compositor: state.compositor.clone(),
                xdg,
                connection: connection.clone(),
                instance: state.instance.clone(),
                viewporter: state.viewporter.clone(),
                fractional_scales: state.fractional_scales.clone(),
                cursors: state.cursors.clone(),
                serial: state.serial.clone(),
                parent: Mutex::default(),
                seat: Mutex::default(),
                last_press: Mutex::default(),
                open: Mutex::default(),
            });
        }
        Err(_) => eprintln!("warning: the compositor has no xdg-shell; there will be no popup surfaces"),
    }
    let _ = LOCKS.set(Locks {
        qh: qh.clone(),
        compositor: state.compositor.clone(),
        manager: smithay_client_toolkit::session_lock::SessionLockState::new(&globals, &qh),
        connection: connection.clone(),
        instance: state.instance.clone(),
        viewporter: state.viewporter.clone(),
        fractional_scales: state.fractional_scales.clone(),
        monitors: Mutex::default(),
        engaged: Mutex::default(),
        shm: globals.bind::<WlShm, _, _>(&qh, 1..=1, Silent).ok(),
    });
    if state.viewporter.is_none() || state.fractional_scales.is_none() {
        eprintln!("warning: the compositor gives no fractional scale; it will paint at whatever whole scale it says");
    }
    // The monitors already there arrive with the first round trips; those
    // plugged in later, through `new_output`.
    events.roundtrip(&mut state).unwrap();
    events.roundtrip(&mut state).unwrap();
    for output in state.outputs.outputs().collect::<Vec<_>>() {
        state.place_on(&output, &qh);
    }
    if state.placed.is_empty() {
        eprintln!("warning: none of the monitors asked for ({:?}) is plugged in; waiting for one to appear", state.wanted.iter().map(|s| &s.screens).collect::<Vec<_>>());
    }
    while !state.quit {
        // The compositor went away (its session ended): nothing left to show on.
        if let Err(e) = events.blocking_dispatch(&mut state) {
            // Leaving the way everything leaves: the render first, then the process.
            eprintln!("wayland · the compositor is gone ({e}): leaving");
            return;
        }
    }
}

impl State {
    /// A surface was closed: its monitor left, or someone closed the window.
    fn surface_gone(&mut self, wl: &wl_surface::WlSurface) {
        if let Some(p) = self.placed.iter().find(|p| p.role.wl() == wl) {
            let _ = self.to_render.send(ToRender::SheetGone(p.id));
        }
        self.placed.retain(|p| p.role.wl() != wl);
        if let Some(c) = MOVABLE_LAYERS.get() {
            c.placed.lock().unwrap().retain(|(_, layer, _, _)| layer.wl_surface() != wl);
        }
    }

    /// Until the compositor configures it nothing can be attached to it: it's now
    /// that it passes into the render's hands. It works the same for a panel and for a window.
    fn configured(&mut self, wl: &wl_surface::WlSurface, new: (u32, u32)) {
        let Some(p) = self.placed.iter_mut().find(|p| p.role.wl() == wl) else { return };
        let its = &self.wanted[p.which];
        let asked = (its.width, its.height + if p.which == 0 { self.extra_height } else { 0 });
        let origin = its.origin;
        // Whatever the compositor gave; if it says 0, what was asked for.
        let size = (if new.0 > 0 { new.0 } else { asked.0 }, if new.1 > 0 { new.1 } else { asked.1 });
        if let Some(v) = &p.viewport {
            v.set_destination(size.0 as i32, size.1 as i32);
        }
        // Where it lands inside its monitor: what's needed to photograph what's behind.
        if let Some(d) = &p.backdrop {
            *d.size.lock().unwrap() = size;
            *d.last_asked.lock().unwrap() = None;
        }
        if let (Some((anchor, margin)), Some(d)) = (p.layer_placement, &p.backdrop) {
            if let Some((w, h)) = self.outputs.info(&p.output).and_then(|i| i.logical_size) {
                *d.position.lock().unwrap() = Some(position_on_output(anchor, margin, size, (w, h)));
                // And let the next capture ask the compositor whether it's true.
                *d.last_asked.lock().unwrap() = None;
            }
        }
        // It had already been handed over and has changed size: someone stretched the window.
        if p.pending.is_none() && p.size != size {
            let _ = self.to_render.send(ToRender::SheetSize(p.id, (size.0 as f32, size.1 as f32)));
        }
        p.size = size;
        if let Some((surface, name, mhz)) = p.pending.take() {
            if let (Some(pp), Some(layer)) = (POPUPS.get(), p.role.layer()) {
                // While the mouse hasn't been seen on any, popups hang from the first one.
                pp.parent.lock().unwrap().get_or_insert_with(|| Parent { layer: layer.clone(), backdrop: p.backdrop.clone(), scale: p.scale, name: name.clone(), mhz });
            }
            let _ = self.to_render.send(ToRender::Sheet(Box::new(gpu::NewSheet {
                id: p.id,
                target: gpu::Target::Surface(surface),
                window: Box::new(WaylandWindow { wl: p.role.wl().clone(), compositor: self.compositor.clone(), cursors: self.cursors.clone(), serial: self.serial.clone(), layer: p.role.layer().cloned(), qh: self.qh.clone(), id: p.id, effect: Mutex::new(None), backdrop: p.backdrop.clone(), shown: Default::default() }),
                scale: p.scale,
                size,
                mhz,
                name,
                view: gpu::View { surface: p.which, popup: None, origin, size: (size.0 as f32, size.1 as f32) },
            })));
        }
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        self.surface_gone(layer.wl_surface());
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface, conf: LayerSurfaceConfigure, _: u32) {
        self.configured(&layer.wl_surface().clone(), conf.new_size);
    }
}

impl smithay_client_toolkit::session_lock::SessionLockHandler for State {
    /// The compositor confirms it: now, and not before, the session is locked.
    fn locked(&mut self, _: &Connection, _: &QueueHandle<Self>, _: smithay_client_toolkit::session_lock::SessionLock) {
        println!("lock   · the session is locked");
        let _ = self.to_render.send(ToRender::LockScreen(true));
    }
    /// It didn't grant it —there's already another locker—, or it has ended it.
    fn finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: smithay_client_toolkit::session_lock::SessionLock) {
        eprintln!("lock   · the compositor did not grant the lock, or ended it: is another locker running?");
        let _ = self.to_render.send(ToRender::LockScreen(false));
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: smithay_client_toolkit::session_lock::SessionLockSurface,
        conf: smithay_client_toolkit::session_lock::SessionLockSurfaceConfigure,
        _: u32,
    ) {
        let Some(c) = LOCKS.get() else { return };
        let mut engaged = c.engaged.lock().unwrap();
        let Some(e) = engaged.as_mut() else { return };
        let (which, bounds, origin) = (e.which, e.bounds, e.origin);
        let Some(face) = e.faces.iter_mut().find(|k| k.surface.wl_surface() == surface.wl_surface()) else { return };
        let size = conf.new_size;
        // A cover: black, as big as its monitor. Zeroed memory is black in XRGB.
        if let (Some(cover), Some(shm)) = (face.cover.as_mut(), c.shm.as_ref()) {
            let len = size.0 as usize * size.1 as usize * 4;
            if cover.as_ref().is_none_or(|(_, _, _, was)| *was != size) && len > 0 {
                let Some(fd) = memfd(len) else { return };
                let pool = shm.create_pool(std::os::fd::AsFd::as_fd(&fd), len as i32, &self.qh, Silent);
                let buffer = pool.create_buffer(0, size.0 as i32, size.1 as i32, size.0 as i32 * 4, wl_shm::Format::Xrgb8888, &self.qh, Silent);
                *cover = Some((pool, buffer, fd, size));
            }
            if let Some((_, b, _, _)) = cover {
                let wl = face.surface.wl_surface();
                wl.attach(Some(b), 0, 0);
                wl.damage_buffer(0, 0, size.0 as i32, size.1 as i32);
                wl.commit();
            }
            return;
        }
        if let Some(v) = &face.viewport {
            v.set_destination(size.0 as i32, size.1 as i32);
        }
        // Its box, centred on THIS monitor: what's left over around it is also
        // seen, so the background is painted large and each monitor shows its own part.
        face.view_origin = (origin.0 - (size.0 as f32 - bounds.0 as f32) / 2.0, origin.1 - (size.1 as f32 - bounds.1 as f32) / 2.0);
        if let Some(paints) = face.pending.take() {
            let _ = self.to_render.send(ToRender::Sheet(Box::new(gpu::NewSheet {
                id: face.id,
                target: gpu::Target::Surface(paints),
                window: Box::new(WaylandWindow { wl: face.surface.wl_surface().clone(), compositor: self.compositor.clone(), cursors: self.cursors.clone(), serial: self.serial.clone(), layer: None, qh: self.qh.clone(), id: face.id, effect: Mutex::new(None), backdrop: None, shown: Default::default() }),
                scale: 1.0,
                size,
                mhz: face.mhz,
                name: format!("{} (lock)", face.name),
                view: gpu::View { surface: which, popup: None, origin: face.view_origin, size: (size.0 as f32, size.1 as f32) },
            })));
        }
    }
}

impl PointerHandler for State {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, pointer: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        // An agent's own seat (`cua-agent`, `cua-agent-2`): its presses are said to be its.
        let from_agent = self.pointer_seats.iter().find(|(p, _)| p == pointer).and_then(|(_, s)| self.seats.info(s)).and_then(|i| i.name).is_some_and(|n| n.starts_with("cua-agent"));
        for e in events {
            let (mut x, mut y) = (e.position.0 as f32, e.position.1 as f32);
            if let Some(pp) = POPUPS.get() {
                // Inside a popup, the mouse is in the piece of scene it shows.
                let on_lock = LOCKS.get().and_then(|c| c.engaged.lock().unwrap().as_ref().and_then(|ec| ec.faces.iter().find(|k| k.surface.wl_surface() == &e.surface).map(|k| k.view_origin)));
                if let Some(view_origin) = on_lock {
                    (x, y) = (x + view_origin.0, y + view_origin.1);
                } else if let Some(a) = pp.open.lock().unwrap().iter().find(|a| a.popup.wl_surface() == &e.surface) {
                    (x, y) = (x + a.origin.0, y + a.origin.1);
                } else if let Some(p) = self.placed.iter().find(|p| p.role.wl() == &e.surface) {
                    let info = self.outputs.info(&p.output);
                    // The mouse over a surface is in the piece of the plane it shows.
                    let origin = self.wanted[p.which].origin;
                    (x, y) = (x + origin.0, y + origin.1);
                    // Popups hang from a layer: from a normal window, not yet.
                    if let Some(layer) = p.role.layer() {
                        *pp.parent.lock().unwrap() = Some(Parent {
                            layer: layer.clone(),
                            backdrop: p.backdrop.clone(),
                            scale: p.scale,
                            name: info.as_ref().and_then(|i| i.name.clone()).unwrap_or_default(),
                            mhz: info.as_ref().and_then(|i| i.modes.iter().find(|m| m.current).map(|m| m.refresh_rate)).unwrap_or(0),
                        });
                    }
                }
                if let PointerEventKind::Press { serial, .. } = e.kind {
                    *pp.last_press.lock().unwrap() = Some((serial, std::time::Instant::now()));
                }
            }
            // A drag out starts from the press that began it: the compositor asks for its serial.
            if let (PointerEventKind::Press { serial, .. }, Some(d)) = (&e.kind, DRAGS.get()) {
                *d.press.lock().unwrap() = Some((*serial, e.surface.clone()));
            }
            // `PLEAMAR_DEBUG_ZONES=1`: which surface the compositor gives the mouse to,
            // when it enters, leaves or presses —not every motion—.
            if std::env::var_os("PLEAMAR_DEBUG_ZONES").is_some() && !matches!(e.kind, PointerEventKind::Motion { .. } | PointerEventKind::Axis { .. }) {
                let which = self.placed.iter().find(|p| p.role.wl() == &e.surface).map(|p| p.which);
                eprintln!("pointer · {:?} on surface {which:?} at {:.0},{:.0}", e.kind, x, y);
            }
            match e.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    if let PointerEventKind::Enter { serial } = e.kind {
                        self.serial.store(serial, Ordering::Relaxed);
                        if let Some(d) = self.cursors.lock().unwrap().as_ref() {
                            let c = Cursor::from_code(CURSOR_SHAPE.load(Ordering::Relaxed));
                            d.set_shape(serial, shape_of(c));
                        }
                    }
                    // The mouse goes to the render, which is the one that knows what's underneath;
                    // it reaches the logic already with a name.
                    let _ = self.to_render.send(ToRender::Pointer(Some((x, y))));
                }
                PointerEventKind::Leave { .. } => {
                    let _ = self.to_render.send(ToRender::Pointer(None));
                }
                PointerEventKind::Press { button, .. } | PointerEventKind::Release { button, .. } => {
                    let down = matches!(e.kind, PointerEventKind::Press { .. });
                    // BTN_LEFT, BTN_RIGHT, BTN_MIDDLE; and the side ones (BTN_SIDE,
                    // BTN_EXTRA, BTN_FORWARD, BTN_BACK): back and forward.
                    let btn = match button {
                        0x110 => 0,
                        0x111 => 1,
                        0x112 => 2,
                        0x113..=0x116 => (button - 0x110) as u8,
                        _ => continue,
                    };
                    // As long as a scene makes no use of the right button, it closes: it's the
                    // emergency exit of a prototype without a keyboard. A scene that uses it
                    // is not a quick prototype, and a right click never closes it.
                    if btn == 1 && down && self.wanted.iter().all(|s| s.right_click_quits) {
                        self.quit = true;
                        continue;
                    }
                    let _ = self.to_render.send(ToRender::Pointer(Some((x, y))));
                    let _ = self.to_render.send(if from_agent { ToRender::AgentButton(btn, down) } else { ToRender::Button(btn, down) });
                }
                PointerEventKind::Axis { vertical, horizontal, .. } => {
                    // Wheel notches, positive upwards. A real wheel sends
                    // 120 per notch; a touchpad, pixels.
                    let notches = |a: &smithay_client_toolkit::seat::pointer::AxisScroll| {
                        if a.value120 != 0 { a.value120 as f32 / 120.0 } else if a.discrete != 0 { a.discrete as f32 } else { a.absolute as f32 / 15.0 }
                    };
                    let d = -(notches(&vertical) + notches(&horizontal));
                    if d != 0.0 {
                        let _ = self.to_render.send(ToRender::Wheel(d));
                    }
                }
            }
        }
    }
}

/// What we know how to receive, in order of preference: files, and otherwise, text.
const MIME_TYPES: [&str; 3] = ["text/uri-list", "text/plain;charset=utf-8", "text/plain"];

fn current_drag_offer(d: &wayland_client::protocol::wl_data_device::WlDataDevice) -> Option<DragOffer> {
    d.data::<DataDeviceData>()?.drag_offer()
}

/// Dragging from another application. While it lasts, the compositor doesn't send the
/// mouse the usual way: it arrives through here, and gets forwarded the same.
impl DataDeviceHandler for State {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, d: &wayland_client::protocol::wl_data_device::WlDataDevice, x: f64, y: f64, _: &wl_surface::WlSurface) {
        if let Some(o) = current_drag_offer(d) {
            let mime = o.with_mime_types(|t| MIME_TYPES.iter().find(|q| t.iter().any(|x| x == *q)).map(|q| q.to_string()));
            o.accept_mime_type(o.serial, mime);
            o.set_actions(DndAction::Copy, DndAction::Copy);
        }
        // `drag.over`: something is being carried over the surface, not
        // yet let go. A scene can open its arms before it arrives.
        let _ = self.to_render.send(ToRender::Fact("drag.over", 1.0));
        let _ = self.to_render.send(ToRender::Pointer(Some((x as f32, y as f32))));
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_data_device::WlDataDevice) {
        // Let go on it, it stays over until what was let go has arrived: the
        // zones that receive it may only be there while something is over.
        // (And where it was let go too: the zone it lands on is the one under it.)
        if !DROPPING.load(std::sync::atomic::Ordering::Acquire) {
            let _ = self.to_render.send(ToRender::Fact("drag.over", 0.0));
            let _ = self.to_render.send(ToRender::Pointer(None));
        }
    }
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_data_device::WlDataDevice, x: f64, y: f64) {
        let _ = self.to_render.send(ToRender::Pointer(Some((x as f32, y as f32))));
    }
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_data_device::WlDataDevice) {}
    fn drop_performed(&mut self, conn: &Connection, _: &QueueHandle<Self>, d: &wayland_client::protocol::wl_data_device::WlDataDevice) {
        let over_no_more = |tx: &std::sync::mpsc::Sender<ToRender>| {
            DROPPING.store(false, std::sync::atomic::Ordering::Release);
            let _ = tx.send(ToRender::Fact("drag.over", 0.0));
        };
        let Some(o) = current_drag_offer(d) else { return over_no_more(&self.to_render) };
        let Some(mime) = o.with_mime_types(|t| MIME_TYPES.iter().find(|q| t.iter().any(|x| x == *q)).map(|q| q.to_string())) else { return over_no_more(&self.to_render) };
        let Ok(mut pipe) = o.receive(mime.clone()) else { return over_no_more(&self.to_render) };
        DROPPING.store(true, std::sync::atomic::Ordering::Release);
        let _ = conn.flush();
        // Reading the pipe can take as long as the writer takes: in another thread.
        let tx = self.to_render.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut data = String::new();
            let _ = pipe.read_to_string(&mut data);
            o.finish();
            o.destroy();
            let _ = tx.send(ToRender::Dropped(mime, data.trim_end().to_owned()));
            DROPPING.store(false, std::sync::atomic::Ordering::Release);
            let _ = tx.send(ToRender::Fact("drag.over", 0.0));
            let _ = tx.send(ToRender::Pointer(None));
        });
    }
}

impl DataOfferHandler for State {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, o: &mut DragOffer, _: DndAction) {
        o.set_actions(DndAction::Copy, DndAction::Copy);
    }
    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

/// What is dragged out of the scene (a zone with `carries:`): the data
/// device to start it on, the press it starts from, and while it lasts, its
/// source and what it gives for each kind asked.
struct Drags {
    connection: Connection,
    qh: QueueHandle<State>,
    manager: wayland_client::protocol::wl_data_device_manager::WlDataDeviceManager,
    device: Mutex<Option<wayland_client::protocol::wl_data_device::WlDataDevice>>,
    press: Mutex<Option<(u32, wl_surface::WlSurface)>>,
    under_way: Mutex<Option<(wayland_client::protocol::wl_data_source::WlDataSource, Vec<(String, Vec<u8>)>)>>,
    /// What makes the picture under the pointer while it lasts (`carry_icon:`): the compositor and shm to make it with, and what it is made of until it ends.
    compositor: CompositorState,
    shm: Option<WlShm>,
    icon: Mutex<Option<(wl_surface::WlSurface, WlBuffer, WlShmPool, std::os::fd::OwnedFd)>>,
}
static DRAGS: std::sync::OnceLock<Drags> = std::sync::OnceLock::new();

/// Starts dragging `text` out to whatever program it is let go on. Files and
/// links go as a list of addresses (and as their text); anything else, as text.
pub fn start_drag(text: &str, icon: Option<&str>) -> bool {
    let Some(d) = DRAGS.get() else { return false };
    let Some((serial, origin)) = d.press.lock().unwrap().clone() else { return false };
    let Some(device) = d.device.lock().unwrap().clone() else { return false };
    let offers = super::drag_offers(text);
    if offers.is_empty() {
        return false;
    }
    let source = d.manager.create_data_source(&d.qh, smithay_client_toolkit::data_device_manager::data_source::DataSourceData::<()>::default());
    for (mime, _) in &offers {
        source.offer(mime.clone());
    }
    if source.version() >= 3 {
        source.set_actions(DndAction::Copy);
    }
    // The picture under the pointer, if the zone gave one and it can be read: its top left is a little up and to the left of the pointer, so the pointer is not hidden.
    let held = icon.and_then(|path| drag_icon(d, path));
    device.start_drag(Some(&source), &origin, held.as_ref().map(|(s, _, _, _)| s), serial);
    if let Some(old) = d.icon.lock().unwrap().take() {
        drop_icon(old);
    }
    *d.icon.lock().unwrap() = held;
    let _ = d.connection.flush();
    if let Some((old, _)) = d.under_way.lock().unwrap().replace((source, offers)) {
        old.destroy();
    }
    true
}

impl DataSourceHandler for State {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_data_source::WlDataSource, _: Option<String>) {}
    // The program it was let go on asks for it in one of the kinds offered.
    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &wayland_client::protocol::wl_data_source::WlDataSource, mime: String, mut pipe: WritePipe) {
        let Some(d) = DRAGS.get() else { return };
        let data = d.under_way.lock().unwrap().as_ref().filter(|(s, _)| s == source).and_then(|(_, o)| o.iter().find(|(m, _)| *m == mime).map(|(_, b)| b.clone()));
        if let Some(bytes) = data {
            let _ = std::io::Write::write_all(&mut pipe, &bytes);
        }
    }
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &wayland_client::protocol::wl_data_source::WlDataSource) {
        end_drag(source);
    }
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_data_source::WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &wayland_client::protocol::wl_data_source::WlDataSource) {
        end_drag(source);
    }
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_data_source::WlDataSource, _: DndAction) {}
}

fn end_drag(source: &wayland_client::protocol::wl_data_source::WlDataSource) {
    let Some(d) = DRAGS.get() else { return };
    let mut under_way = d.under_way.lock().unwrap();
    if under_way.as_ref().is_some_and(|(s, _)| s == source) {
        *under_way = None;
        if let Some(held) = d.icon.lock().unwrap().take() {
            drop_icon(held);
        }
    }
    source.destroy();
}

/// A PNG as the surface of a drag: read, premultiplied (what Argb8888 means) and put in shared memory. Bigger than 512 px a side, or unreadable, it is none.
fn drag_icon(d: &Drags, path: &str) -> Option<(wl_surface::WlSurface, WlBuffer, WlShmPool, std::os::fd::OwnedFd)> {
    let shm = d.shm.as_ref()?;
    let img = image::open(path).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 || w > 512 || h > 512 {
        return None;
    }
    let len = w as usize * h as usize * 4;
    let fd = memfd(len)?;
    // SAFETY: a mapping of the file just made, as long as it, written here and let go before it is used by anybody else.
    unsafe {
        let map = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, std::os::fd::AsRawFd::as_raw_fd(&fd), 0);
        if map == libc::MAP_FAILED {
            return None;
        }
        let out = std::slice::from_raw_parts_mut(map as *mut u8, len);
        for (o, p) in out.chunks_exact_mut(4).zip(img.pixels()) {
            let a = p.0[3] as u32;
            let mul = |c: u8| ((c as u32 * a + 127) / 255) as u8;
            o.copy_from_slice(&[mul(p.0[2]), mul(p.0[1]), mul(p.0[0]), p.0[3]]);
        }
        libc::munmap(map, len);
    }
    let pool = shm.create_pool(std::os::fd::AsFd::as_fd(&fd), len as i32, &d.qh, Silent);
    let buffer = pool.create_buffer(0, w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888, &d.qh, Silent);
    let wl = d.compositor.create_surface(&d.qh);
    let (hx, hy) = (-(w.min(28) as i32), -(h.min(28) as i32));
    if wl.version() >= 5 {
        wl.attach(Some(&buffer), 0, 0);
        wl.offset(hx, hy);
    } else {
        wl.attach(Some(&buffer), hx, hy);
    }
    wl.damage_buffer(0, 0, w as i32, h as i32);
    wl.commit();
    Some((wl, buffer, pool, fd))
}

fn drop_icon((surface, buffer, pool, _fd): (wl_surface::WlSurface, WlBuffer, WlShmPool, std::os::fd::OwnedFd)) {
    surface.destroy();
    buffer.destroy();
    pool.destroy();
}

/// Something let go on a surface whose data has not arrived yet.
static DROPPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn key_name(e: &KeyEvent) -> String {
    // As xkb calls it: `Escape`, `Return`, `BackSpace`, `a`.
    e.keysym.name().map(|n| n.trim_start_matches("XK_").to_owned()).unwrap_or_else(|| format!("{:#x}", e.keysym.raw()))
}

impl KeyboardHandler for State {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {
        let _ = self.to_render.send(ToRender::KeyboardFocus(true));
    }
    /// Losing the keyboard is, almost always, that someone clicked somewhere else.
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {
        let _ = self.to_render.send(ToRender::KeyboardFocus(false));
    }
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_keyboard::WlKeyboard, _: u32, e: KeyEvent) {
        // An arrow, an F key: some compositors say they type "" rather than
        // nothing. Nothing is what they type.
        let typed = e.utf8.clone().filter(|t| !t.is_empty() && !t.chars().any(char::is_control));
        let _ = self.to_render.send(ToRender::Key(key_name(&e), typed, self.mods, e.raw_code));
    }
    /// The layout as the user has it: the scene's own compositor hands the
    /// same one to its windows.
    fn update_keymap(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_keyboard::WlKeyboard, keymap: smithay_client_toolkit::seat::keyboard::Keymap<'_>) {
        super::set_host_keymap(keymap.as_string());
    }
    fn update_repeat_info(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_keyboard::WlKeyboard, info: RepeatInfo) {
        let r = match info {
            RepeatInfo::Repeat { rate, delay } => Some((delay, (1000 / rate.get()).max(1))),
            RepeatInfo::Disable => None,
        };
        match r {
            Some((delay, every)) => println!("keyboard · repeats after {delay} ms, then every {every} ms"),
            None => println!("keyboard · no repeat"),
        }
        let _ = self.to_render.send(ToRender::KeyRepeat(r));
    }
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_keyboard::WlKeyboard, _: u32, e: KeyEvent) {
        let _ = self.to_render.send(ToRender::KeyReleased(key_name(&e), e.raw_code));
    }
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_keyboard::WlKeyboard, _: u32, m: Modifiers, _: RawModifiers, _: u32) {
        self.mods = Mods { ctrl: m.ctrl, alt: m.alt, shift: m.shift, logo: m.logo };
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, c: Capability) {
        if let Some(pp) = POPUPS.get() {
            pp.seat.lock().unwrap().get_or_insert_with(|| seat.clone());
        }
        if c == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seats.get_pointer(qh, &seat).ok();
            if let (Some(p), Some(m)) = (&self.pointer, &self.cursor_shapes) {
                *self.cursors.lock().unwrap() = Some(m.get_shape_device(p, qh));
            }
            if let Some(p) = &self.pointer {
                self.pointer_seats.push((p.clone(), seat.clone()));
            }
        } else if c == Capability::Pointer {
            // Another seat's pointer: an agent's, which has hands of its own. It
            // is heard too, so that its presses are known to be its.
            if let Ok(p) = self.seats.get_pointer(qh, &seat) {
                self.pointer_seats.push((p, seat.clone()));
            }
        }
        if self.data_device.is_none() {
            self.data_device = self.drag_and_drop.as_ref().map(|m| m.get_data_device(qh, &seat));
            if let (Some(d), Some(dd)) = (DRAGS.get(), &self.data_device) {
                *d.device.lock().unwrap() = Some(dd.inner().clone());
            }
        }
        if c == Capability::Keyboard && self.keyboard.is_none() && self.wanted.iter().any(|s| s.keyboard != Keyboard::Never) {
            self.keyboard = self.seats.get_keyboard(qh, &seat, None).ok();
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, _: Capability) {}
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl CompositorHandler for State {
    /// The good old whole scale. It only counts if the compositor doesn't give the
    /// fractional one, which arrives by its own path.
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, wl: &wl_surface::WlSurface, scale: i32) {
        if self.fractional_scales.is_some() && self.viewporter.is_some() {
            return;
        }
        if let Some(p) = self.placed.iter_mut().find(|p| p.role.wl() == wl) {
            wl.set_buffer_scale(scale);
            p.scale = scale as f32;
            let _ = self.to_render.send(ToRender::Scale(p.id, scale as f32));
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    /// A monitor plugged in with the program running.
    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        outputs_changed(&self.to_render);
        self.place_on(&output, qh);
        self.place_on_all(qh, None);
    }
    /// What a monitor says about itself (its name, its modes) can arrive after it
    /// does, and one that came without a name was not given its surfaces: here they
    /// are. It is safe to do again: what is already on it is not made twice.
    ///
    /// A monitor changed mode (or scale, or turned). A surface that is as big as its monitor and keeps a reserve asks for the monitor's size along one
    /// edge (see `placement`), and the compositor keeps that size: after a new mode it was still the old one, and the surface stayed as it was
    /// until the program was started again. Ask again with the new size; the compositor answers with a configure, and `configured` resizes the sheet.
    fn update_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.place_on(&output, qh);
        let Some((w, h)) = self.outputs.info(&output).and_then(|i| i.logical_size) else { return };
        let Some(c) = MOVABLE_LAYERS.get() else { return };
        let mut any = false;
        for (_, layer, _, stretch) in c.placed.lock().unwrap().iter_mut() {
            if !(stretch.width && stretch.height) || stretch.monitor.is_none() || stretch.monitor == Some((w, h)) {
                continue;
            }
            if !self.placed.iter().any(|p| p.output == output && p.role.wl() == layer.wl_surface()) {
                continue;
            }
            stretch.monitor = Some((w, h));
            let (up, down) = (stretch.edges.contains(Anchor::TOP), stretch.edges.contains(Anchor::BOTTOM));
            let (left, right) = (stretch.edges.contains(Anchor::LEFT), stretch.edges.contains(Anchor::RIGHT));
            let size = if up && down && !(left && right) {
                (w.max(0) as u32, 0)
            } else if left && right && !(up && down) {
                (0, h.max(0) as u32)
            } else {
                continue;
            };
            layer.set_size(size.0, size.1);
            layer.commit();
            any = true;
        }
        if any {
            let _ = c.connection.flush();
        }
    }
    fn output_destroyed(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        outputs_changed(&self.to_render);
        self.remove_from(&output);
        self.place_on_all(qh, Some(&output));
    }
}

delegate_registry!(State);
impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(State);

/// An icon by its name, the freedesktop way: the user's GTK icon theme and
/// the ones it inherits, then hicolor and a few common ones, in every icon
/// folder of `XDG_DATA_HOME` and `XDG_DATA_DIRS`, each theme's folders as
/// its `index.theme` lists them, from vector down to small.
///
/// The folders are read once and kept, with only the ones that exist: one
/// made afterwards (an app installed since, the first with an icon of its
/// size) would never be looked at. So a name that is not found reads them
/// again, at most every five seconds, and is looked for there.
pub fn icon(name: &str) -> Option<std::path::PathBuf> {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    static DIRS: Mutex<Option<(Instant, Arc<Vec<std::path::PathBuf>>)>> = Mutex::new(None);
    let find = |dirs: &[std::path::PathBuf]| dirs.iter().flat_map(|d| ["svg", "png"].map(|e| d.join(format!("{name}.{e}")))).find(|p| p.is_file());
    let (read, dirs) = {
        let mut kept = DIRS.lock().unwrap();
        let (read, dirs) = kept.get_or_insert_with(|| (Instant::now(), Arc::new(icon_dirs())));
        (*read, dirs.clone())
    };
    if let Some(found) = find(&dirs) {
        return Some(found);
    }
    if read.elapsed() < Duration::from_secs(5) {
        return None;
    }
    let fresh = Arc::new(icon_dirs());
    *DIRS.lock().unwrap() = Some((Instant::now(), fresh.clone()));
    find(&fresh)
}

fn icon_dirs() -> Vec<std::path::PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let data_home = std::env::var("XDG_DATA_HOME").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| format!("{home}/.local/share"));
    let data_dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let config = std::env::var("XDG_CONFIG_HOME").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| format!("{home}/.config"));
    icon_dirs_in(&home, &data_home, &data_dirs, &config)
}

/// The same, from where the home, the data folders and the settings are.
fn icon_dirs_in(home: &str, data_home: &str, data_dirs: &str, config: &str) -> Vec<std::path::PathBuf> {
    use std::path::{Path, PathBuf};
    let mut shares = vec![PathBuf::from(data_home)];
    shares.extend(data_dirs.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    shares.push("/usr/share".into());
    // A folder listed twice is looked at once, where it came first.
    let mut seen = std::collections::HashSet::new();
    shares.retain(|s| seen.insert(s.clone()));
    let mut bases = vec![PathBuf::from(data_home).join("icons"), PathBuf::from(format!("{home}/.icons"))];
    bases.extend(shares.iter().map(|s| s.join("icons")));
    let mut seen = std::collections::HashSet::new();
    bases.retain(|b| seen.insert(b.clone()));
    let index = |theme: &str| bases.iter().map(|b| b.join(theme).join("index.theme")).find(|p| p.is_file()).and_then(|p| std::fs::read_to_string(p).ok());
    let key = |text: &str, section: &str, key: &str| -> Option<String> {
        let mut inside = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                inside = line == format!("[{section}]");
            } else if inside {
                if let Some((k, v)) = line.split_once('=') {
                    if k.trim() == key {
                        return Some(v.trim().to_owned());
                    }
                }
            }
        }
        None
    };

    // The theme GTK is set to, then what it inherits, then the fallbacks.
    let user = ["gtk-4.0", "gtk-3.0"].iter()
        .filter_map(|g| std::fs::read_to_string(format!("{config}/{g}/settings.ini")).ok())
        .find_map(|t| key(&t, "Settings", "gtk-icon-theme-name"));
    let mut themes: Vec<String> = Vec::new();
    let mut queue: Vec<String> = user.into_iter().collect();
    while let Some(theme) = queue.pop() {
        if themes.contains(&theme) {
            continue;
        }
        let parents = index(&theme).and_then(|t| key(&t, "Icon Theme", "Inherits")).unwrap_or_default();
        themes.push(theme);
        queue.extend(parents.split(',').map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()).rev());
    }
    for theme in ["hicolor", "Papirus", "Papirus-Dark", "Adwaita", "breeze", "breeze-dark"] {
        if !themes.iter().any(|t| t == theme) {
            themes.push(theme.into());
        }
    }

    let mut dirs = Vec::new();
    for theme in &themes {
        // Scalable first, then the largest.
        let subdirs: Vec<String> = match index(theme) {
            Some(text) => {
                let mut listed: Vec<(bool, u32, String)> = key(&text, "Icon Theme", "Directories").unwrap_or_default()
                    .split(',').map(str::trim).filter(|d| !d.is_empty())
                    .map(|d| {
                        let scalable = key(&text, d, "Type").is_some_and(|t| t == "Scalable");
                        let size = key(&text, d, "Size").and_then(|s| s.parse().ok()).unwrap_or(0);
                        (scalable, size, d.to_owned())
                    })
                    .collect();
                listed.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
                listed.into_iter().map(|(_, _, d)| d).collect()
            }
            None => {
                let sizes = ["scalable", "512x512", "256x256", "128x128", "96x96", "64x64", "48x48", "32x32", "symbolic"];
                let contexts = ["apps", "devices", "places", "status", "actions", "categories", "mimetypes"];
                // Some themes order by size/context and others by context/size.
                sizes.iter().flat_map(|s| contexts.iter().flat_map(move |c| [format!("{s}/{c}"), format!("{c}/{s}")])).collect()
            }
        };
        for base in &bases {
            let root = base.join(theme);
            if root.is_dir() {
                dirs.extend(subdirs.iter().map(|d| root.join(d)).filter(|d| d.is_dir()));
            }
        }
    }
    dirs.extend(shares.iter().map(|s| s.join("pixmaps")).filter(|d| Path::new(d).is_dir()));
    dirs
}

#[cfg(test)]
mod tests {
    use super::icon_dirs_in;

    fn file(path: &std::path::Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// The GTK theme comes first, then what it inherits, then hicolor; each
    /// theme in the folders its `index.theme` lists, vector first, then the
    /// largest; and a theme is looked for in every data folder, not only
    /// /usr/share (NixOS has none there).
    #[test]
    fn icons_are_found_the_freedesktop_way() {
        let root = std::env::temp_dir().join(format!("pleamar-icons-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (home, profile, system, config) = (root.join("home"), root.join("profile"), root.join("system"), root.join("config"));
        file(&config.join("gtk-3.0/settings.ini"), "[Settings]\ngtk-icon-theme-name=Mine-dark\n");
        file(&profile.join("icons/Mine-dark/index.theme"), "[Icon Theme]\nInherits=Mine\nDirectories=16/apps\n\n[16/apps]\nSize=16\n");
        file(&profile.join("icons/Mine-dark/16/apps/term.svg"), "");
        file(&profile.join("icons/Mine/index.theme"), "[Icon Theme]\nInherits=hicolor\nDirectories=16/apps,256/apps,scalable/apps\n\n[16/apps]\nSize=16\n\n[256/apps]\nSize=256\n\n[scalable/apps]\nSize=64\nType=Scalable\n");
        for d in ["16/apps", "256/apps", "scalable/apps"] {
            file(&profile.join(format!("icons/Mine/{d}/term.svg")), "");
        }
        file(&system.join("icons/hicolor/48x48/apps/term.png"), "");
        let data_dirs = format!("{}:{}", profile.display(), system.display());
        let dirs = icon_dirs_in(home.to_str().unwrap(), home.join(".local/share").to_str().unwrap(), &data_dirs, config.to_str().unwrap());
        let ours: Vec<String> = dirs.iter().filter(|d| d.starts_with(&root)).map(|d| d.strip_prefix(&root).unwrap().display().to_string()).collect();
        assert_eq!(
            ours,
            [
                "profile/icons/Mine-dark/16/apps",
                "profile/icons/Mine/scalable/apps",
                "profile/icons/Mine/256/apps",
                "profile/icons/Mine/16/apps",
                "system/icons/hicolor/48x48/apps",
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A folder that comes up twice (the data home also in XDG_DATA_DIRS,
    /// /usr/share also at the end) is walked once.
    #[test]
    fn no_icon_folder_comes_twice() {
        let root = std::env::temp_dir().join(format!("pleamar-icons-twice-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (home, data_home, config) = (root.join("home"), root.join("data"), root.join("config"));
        file(&data_home.join("icons/hicolor/48x48/apps/a.png"), "");
        file(&data_home.join("pixmaps/b.png"), "");
        let data_dirs = format!("{0}:/usr/share:{0}:/usr/share", data_home.display());
        let dirs = icon_dirs_in(home.to_str().unwrap(), data_home.to_str().unwrap(), &data_dirs, config.to_str().unwrap());
        let mut seen = std::collections::HashSet::new();
        assert!(dirs.iter().all(|d| seen.insert(d)), "{dirs:?}");
        assert!(dirs.contains(&data_home.join("icons/hicolor/48x48/apps")));
        assert!(dirs.contains(&data_home.join("pixmaps")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
