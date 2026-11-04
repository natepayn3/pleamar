//! The lens: seeing what is behind the glass so it can be bent.
//!
//! Real glass does not show what is behind it as is: near the edge it bends
//! it, like a drop. That needs the pixels behind, and in Wayland only the
//! compositor has them. It is asked for a capture of the screen where the
//! surface is —with the surface on top, which is the only thing it can give—
//! and it gets unmixed: capture = ours + (1 − our alpha) · background, and we
//! know ours because we paint it on a canvas before presenting it. Measured:
//! the unmixed background is off from the real one by less than half a level
//! out of 255 with a 30 % tint, and by less than two with an 85 % edge.
//!
//! With the background already clean it is blurred a little —the frosting of
//! the glass— and the shapes shader reads it displaced according to the bevel.
//! So pleamar paints the lens itself, without the compositor knowing anything
//! about glass.

use crate::platform::Backdrop;

// How what is behind gets frosted is the scene's: see `blur.rs`.

pub struct Pipelines {
    unmix: wgpu::RenderPipeline,
    /// Unmixing into a draft while counting what says the capture is of
    /// another frame, and keeping the draft only if almost nothing did
    /// (`backdrop_check.wgsl`); none where a fragment cannot count (OpenGL).
    checked: Option<(wgpu::RenderPipeline, wgpu::RenderPipeline)>,
    own: wgpu::RenderPipeline,
    own_over: wgpu::RenderPipeline,
    linear: wgpu::Sampler,
}

impl Pipelines {
    pub fn new(d: &wgpu::Device) -> Pipelines {
        let module = d.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("backdrop"), source: wgpu::ShaderSource::Wgsl(include_str!("backdrop.wgsl").into()) });
        let pipeline_to = |fs: &str, format: wgpu::TextureFormat| {
            d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(fs),
                layout: None,
                vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipeline = |fs: &str| pipeline_to(fs, wgpu::TextureFormat::Rgba8Unorm);
        let linear = d.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        // The same file and the counting, as one module: only where it builds.
        let checked = {
            let scope = d.push_error_scope(wgpu::ErrorFilter::Validation);
            let source = format!("{}{}", include_str!("backdrop.wgsl"), include_str!("backdrop_check.wgsl"));
            let module = d.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("backdrop, checked"), source: wgpu::ShaderSource::Wgsl(source.into()) });
            let make = |fs: &str| {
                d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(fs),
                    layout: None,
                    vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some(fs),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState { format: wgpu::TextureFormat::Rgba8Unorm, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            };
            let both = (make("unmix_checked"), make("settle"));
            match pollster::block_on(scope.pop()) {
                None => Some(both),
                Some(e) => {
                    eprintln!("render · the glass unmixes each capture without checking it is of the frame it was taken with ({e})");
                    None
                }
            }
        };
        Pipelines {
            unmix: pipeline("unmix"),
            checked,
            own: pipeline("own"),
            own_over: pipeline("own_over"),
            linear,
        }
    }
}

pub fn texture(d: &wgpu::Device, label: &str, (w, h): (u32, u32), format: wgpu::TextureFormat, usage: wgpu::TextureUsages) -> (wgpu::Texture, wgpu::TextureView) {
    let t = d.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    });
    let v = t.create_view(&Default::default());
    (t, v)
}

/// What a surface with glass has: where each frame is painted before
/// presenting it, the last capture, and the unmixed background —sharp and
/// frosted—.
/// How long a new lens keeps capturing back to back before it relies on
/// «only when something changes», and at least how many captures. The compositor
/// fades a layer in when it appears —a few hundred milliseconds in which its real
/// opacity is less than 1—, and what is unmixed from those captures is wrong:
/// by a little under something dark, by everything under something bright.
/// Something that keeps moving replaces it at once; a glass or a shader that
/// stays still from its first frame kept it for ever.
pub const WARM_UP: u32 = 3;
pub const WARM_UP_TIME: std::time::Duration = std::time::Duration::from_millis(1500);

pub struct Lens {
    pub px: (u32, u32),
    /// Two canvases: one gets painted, the other is the one the compositor has.
    canvases: [(wgpu::Texture, wgpu::TextureView); 2],
    next: usize,
    presented: Option<usize>,
    capture: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    /// The unmixed background, in two to go back and forth: each time the one
    /// that is not the current one is written, reading the current one where
    /// it cannot be unmixed.
    backgrounds: [(wgpu::Texture, wgpu::TextureView); 2],
    current: usize,
    /// What a capture unmixes to before it is known to be of the right frame,
    /// and how many pixels were looked at and how many said it is not
    /// (`Pipelines::checked`).
    draft: (wgpu::Texture, wgpu::TextureView),
    verdict: wgpu::Buffer,
    /// `PLEAMAR_LENS_LOG`: where the verdicts of the capture being unmixed are copied, to be read back and said
    /// (one pair of counts per part), and the canvas it was judged against, kept as a picture; which capture.
    judging: Option<Judging>,
    frost: Frost,
    /// A sample of the last capture, and of which box it was: so as not to
    /// repeat the work if nothing has changed.
    sample: Vec<u8>,
    /// The last capture taken in has been taken in once more, identical: see `receive`.
    confirmed: bool,
    /// What the scene itself painted again (surface pixels, logical) since the last capture taken in was
    /// asked: there a capture differs from that one because of us, not because of what is behind (#125).
    /// `ours_now` is since the last capture asked; `ours_asked`, up to the one on its way. `None`: not known.
    ours_now: Option<Vec<[f32; 4]>>,
    ours_asked: Option<Vec<[f32; 4]>>,
    /// The last capture that came was not the same as the one before it (or was: then the check lets it through).
    pub last_new: bool,
    last_same: bool,
    /// The card's own look at a capture it holds (`receive`), made the first time one arrives.
    compare: Option<Compare>,
    /// `capture` holds, whole, the last capture the compositor wrote on the card: the next one can be compared with it.
    card_kept: bool,
    pub bounds: [i32; 4],
    bounds_uniforms: wgpu::Buffer,
    /// A received capture whose background is still to be unmixed: which
    /// canvas was presented and with which clip.
    pending: Option<(usize, Vec<(u32, u32, u32, u32)>)>,
    /// There is already a background to show.
    pub ready: bool,
    /// How many captures it has taken in, and since when it exists: see `WARM_UP`.
    pub received: u32,
    born: std::time::Instant,
    /// What the shapes shader reads: the sharp background and the frosted one.
    pub group: Option<wgpu::BindGroup>,
    /// The background is a picture (`lens = image`), not a capture: which
    /// version of it is in, and whether it still has to be frosted.
    pub fixed: Option<u64>,
    fixed_frost: bool,
    /// The blur's values it was last frosted with: a picture is frosted again when they change.
    frosted_with: [f32; 8],
}

impl Lens {
    /// The blur plan its frosting was made for.
    pub fn blur_generation(&self) -> u64 {
        self.frost.generation
    }

    pub fn new(d: &wgpu::Device, format: wgpu::TextureFormat, px: (u32, u32), b: &crate::blur::Built) -> Lens {
        use wgpu::TextureUsages as U;
        let canvas = || texture(d, "canvas", px, format, U::RENDER_ATTACHMENT | U::TEXTURE_BINDING | U::COPY_SRC);
        let background = || texture(d, "background", px, wgpu::TextureFormat::Rgba8Unorm, U::RENDER_ATTACHMENT | U::TEXTURE_BINDING | U::COPY_SRC | U::COPY_DST);
        Lens {
            px,
            canvases: [canvas(), canvas()],
            next: 0,
            presented: None,
            capture: None,
            backgrounds: [background(), background()],
            current: 0,
            draft: texture(d, "draft", px, wgpu::TextureFormat::Rgba8Unorm, U::RENDER_ATTACHMENT | U::TEXTURE_BINDING),
            verdict: d.create_buffer(&wgpu::BufferDescriptor { label: Some("verdict"), size: 8, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false }),
            judging: None,
            frost: Frost::new(d, px, b),
            sample: Vec::new(),
            confirmed: false,
            ours_now: Some(Vec::new()),
            ours_asked: Some(Vec::new()),
            last_new: false,
            last_same: false,
            compare: None,
            card_kept: false,
            pending: None,
            bounds: [0; 4],
            bounds_uniforms: d.create_buffer(&wgpu::BufferDescriptor { label: Some("bounds"), size: 32, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }),
            ready: false,
            received: 0,
            born: std::time::Instant::now(),
            group: None,
            fixed: None,
            fixed_frost: false,
            frosted_with: [0.0; 8],
        }
    }

    /// The scene painted these pieces again (surface pixels, logical), or something it does not say where (`None`).
    pub fn painted_ours(&mut self, pieces: Option<&[[f32; 4]]>) {
        self.ours_now = join_ours(self.ours_now.take(), pieces);
    }

    /// A capture was asked for: it sees what was painted up to now.
    pub fn asked(&mut self) {
        let now = std::mem::replace(&mut self.ours_now, Some(Vec::new()));
        self.ours_asked = join_ours(self.ours_asked.take(), now.as_deref());
    }

    /// Which of the two canvases this frame is painted on.
    pub fn next(&self) -> usize {
        self.next
    }

    /// Where this frame is painted.
    pub fn canvas(&self) -> &wgpu::TextureView {
        &self.canvases[self.next].1
    }

    /// What was painted goes to the screen: it is copied, and stays as the presented one.
    pub fn copy_to(&mut self, encoder: &mut wgpu::CommandEncoder, screen: &wgpu::Texture) {
        let (w, h) = self.px;
        encoder.copy_texture_to_texture(
            self.canvases[self.next].0.as_image_copy(),
            screen.as_image_copy(),
            wgpu::Extent3d { width: w.min(screen.width()), height: h.min(screen.height()), depth_or_array_layers: 1 },
        );
        self.presented = Some(self.next);
        self.next ^= 1;
    }

    /// A capture of the box `bounds` arrives (logical pixels of the surface):
    /// the background is unmixed and frosted, only there. `true` if something
    /// visible has changed, and it has to be painted again.
    pub fn receive(&mut self, g: &crate::gpu::Gpu, d: Backdrop, scale: f32, bounds: [i32; 4]) -> bool {
        if self.fixed.is_some() {
            return false;
        }
        let Some(presented) = self.presented else {
            said(|c| c.unpresented += 1);
            return false;
        };
        // A capture without data is one that failed (unless the compositor wrote it on the card).
        let card = d.card.clone();
        if card.is_none() && d.data.is_none() {
            said(|c| c.failed += 1);
            return false;
        }
        self.read_judging(g);
        // Counted even if it brings nothing new: a still background must not
        // keep the warm-up going for ever. During the warm-up none is skipped
        // for looking like the last one: the first ones are the ones not to
        // trust, and with a still scene the next ones look exactly like them.
        self.received += 1;
        // The same as the previous one —or almost: the rounding of going back
        // and forth gives the odd stray level—, and there is nothing new
        // behind. Without this, painting the lens changes the capture, which
        // changes the lens, and so on without end. One byte in thirteen is
        // looked at, which is enough to know whether something has moved.
        // A capture of parts looks at the parts only: the rest of the buffer holds nothing.
        let parts: Vec<[u32; 4]> = d.rects.iter().copied().filter(|r| r[2] > 0 && r[3] > 0 && r[0] + r[2] <= d.width && r[1] + r[3] <= d.height).collect();
        // Where the scene painted again since the capture this one is compared with, in the capture's pixels
        // (a little wider: what is painted spills a pixel or two past its box). The capture has us on top,
        // so a glass that animates itself —a ripple, a spinner— made every capture look new while nothing
        // behind had moved, and Live bend never rested (#125). Those bytes are not compared.
        let ours: Vec<[i64; 4]> = match (&self.ours_asked, bounds == self.bounds) {
            (Some(rects), true) => rects.iter().map(|b| {
                let x0 = ((b[0] - bounds[0] as f32) * scale).floor() as i64 - 2;
                let y0 = ((b[1] - bounds[1] as f32) * scale).floor() as i64 - 2;
                let x1 = ((b[2] - bounds[0] as f32) * scale).ceil() as i64 + 2;
                let y1 = ((b[3] - bounds[1] as f32) * scale).ceil() as i64 + 2;
                [x0, y0, x1, y1]
            }).filter(|r| r[2] > 0 && r[3] > 0 && r[0] < d.width as i64 && r[1] < d.height as i64).collect(),
            _ => Vec::new(),
        };
        // A capture the compositor wrote on the card is looked at there, by the card (`Compare`): one pixel in
        // nine, with the same tolerance and the same boxes of ours left out; the processor never reads it.
        let size = (d.width, d.height);
        let (strictly_same, same, sample, data) = if let Some(card) = &card {
            let comparable = self.card_kept && bounds == self.bounds && self.capture.as_ref().is_some_and(|f| f.2 == size);
            let (strictly_same, same) = if comparable {
                let old = &self.capture.as_ref().unwrap().0;
                let compare = self.compare.get_or_insert_with(|| Compare::new(&g.device));
                compare.same(g, &card.texture, old, size, &ours)
            } else {
                (false, false)
            };
            (strictly_same, same, Vec::new(), None)
        } else {
            let data: &[u8] = (**d.data.as_ref().unwrap()).as_ref();
            let theirs = |at: usize| {
                let (x, y) = (((at % d.stride as usize) / 4) as i64, (at / d.stride as usize) as i64);
                !ours.iter().any(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3])
            };
            let mut behind: Vec<bool> = Vec::new();
            let sample: Vec<u8> = if parts.is_empty() {
                if !ours.is_empty() {
                    behind = (0..data.len()).step_by(13).map(theirs).collect();
                }
                data.iter().copied().step_by(13).collect()
            } else {
                let mut all = Vec::new();
                for r in &parts {
                    for y in r[1]..r[1] + r[3] {
                        let from = (y * d.stride + r[0] * 4) as usize;
                        if let Some(row) = data.get(from..from + r[2] as usize * 4) {
                            all.extend(row.iter().copied().step_by(13));
                            if !ours.is_empty() {
                                behind.extend((from..from + row.len()).step_by(13).map(theirs));
                            }
                        }
                    }
                }
                all
            };
            // But once: the second time the same one comes, it is taken in again as
            // `same`, and the check lets it through (see `settle`). A capture
            // identical to the one before it is not of another frame —nothing has
            // moved—: if the check refused it, what it counted is something of the
            // compositor's on top of the glass —another bar's piece hanging over it,
            // a notice—, which no capture will ever take away, and refusing them all
            // left the glass with its first background for ever, black (#113).
            let alike = |a: &u8, b: &u8| a.abs_diff(*b) <= 3;
            let strictly_same = bounds == self.bounds && sample.len() == self.sample.len() && sample.iter().zip(&self.sample).all(|(a, b)| alike(a, b));
            // What is behind is the same: it differs only where we painted. Not when we painted most of it: then
            // too little is left to tell, and a change behind a glass that animates whole has to be seen.
            let left = behind.iter().filter(|b| **b).count();
            let same = strictly_same
                || (bounds == self.bounds && sample.len() == self.sample.len() && behind.len() == sample.len() && left * 2 > sample.len() && sample.iter().zip(&self.sample).zip(&behind).all(|((a, b), theirs)| !theirs || alike(a, b)));
            (strictly_same, same, sample, Some(data))
        };
        let no = said(|c| {
            c.arrived += 1;
            c.n += 1;
            c.n
        });
        if let Some(no) = no {
            let how = if same && !self.confirmed && self.ready {
                "the same as the one before: taken in once more"
            } else if self.ready && !self.warming() && same {
                "the same as the one before: nothing to do"
            } else if same {
                "the same as the one before, warming up"
            } else {
                "new"
            };
            detail(no, || format!("lens · capture #{no} · {}×{} px{} · opacity {:.2} · {how}", d.width, d.height, if d.rects.is_empty() { String::new() } else { format!(" · {} parts", d.rects.len()) }, d.opacity));
            if let Some(data) = data {
                keep_picture(no, "capture", d.width, d.height, d.stride, data, true);
            }
        }
        if same && !self.confirmed && self.ready {
            self.confirmed = true;
        } else if self.ready && !self.warming() && same {
            said(|c| c.unchanged += 1);
            return false;
        }
        if !same {
            self.confirmed = false;
        }
        self.last_new = !same;
        self.last_same = same;
        self.sample = sample;
        self.bounds = bounds;
        // The one compared with from now on is this one: what we paint counts from it.
        self.ours_asked = Some(Vec::new());
        let dev = &g.device;
        if self.capture.as_ref().is_none_or(|f| f.2 != size) {
            let (tx, v) = texture(dev, "capture", size, wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST);
            self.capture = Some((tx, v, size));
        }
        let capture = self.capture.as_ref().unwrap();
        self.card_kept = card.is_some();
        if let Some(card) = &card {
            let mut enc = dev.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("a capture copied on the card") });
            enc.copy_texture_to_texture(card.texture.as_image_copy(), capture.0.as_image_copy(), wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 });
            g.queue.submit([enc.finish()]);
        } else if parts.is_empty() {
            let data = data.unwrap();
            g.queue.write_texture(
                capture.0.as_image_copy(),
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(d.stride), rows_per_image: Some(d.height) },
                wgpu::Extent3d { width: d.width, height: d.height, depth_or_array_layers: 1 },
            );
        } else {
            // Only what was photographed goes up to the card: the rest of the texture keeps what it had, and is not read.
            let data = data.unwrap();
            for r in &parts {
                let mut to = capture.0.as_image_copy();
                to.origin = wgpu::Origin3d { x: r[0], y: r[1], z: 0 };
                g.queue.write_texture(
                    to,
                    data,
                    wgpu::TexelCopyBufferLayout { offset: (r[1] * d.stride + r[0] * 4) as u64, bytes_per_row: Some(d.stride), rows_per_image: Some(r[3]) },
                    wgpu::Extent3d { width: r[2], height: r[3], depth_or_array_layers: 1 },
                );
            }
        }
        // The box in real pixels, inside the canvas.
        let (lw, lh) = self.px;
        let x0 = ((bounds[0] as f32 * scale).floor() as u32).min(lw);
        let y0 = ((bounds[1] as f32 * scale).floor() as u32).min(lh);
        let x1 = (((bounds[0] + bounds[2]) as f32 * scale).ceil() as u32).min(lw);
        let y1 = (((bounds[1] + bounds[3]) as f32 * scale).ceil() as u32).min(lh);
        if x1 <= x0 || y1 <= y0 {
            return false;
        }
        // What is unmixed: the whole box, or the parts of it that came.
        let scissors: Vec<(u32, u32, u32, u32)> = if parts.is_empty() {
            vec![(x0, y0, x1 - x0, y1 - y0)]
        } else {
            parts.iter().filter_map(|r| {
                let (px0, py0) = ((x0 + r[0]).min(lw), (y0 + r[1]).min(lh));
                let (px1, py1) = ((x0 + r[0] + r[2]).min(lw), (y0 + r[1] + r[3]).min(lh));
                (px1 > px0 && py1 > py0).then_some((px0, py0, px1 - px0, py1 - py0))
            }).collect()
        };
        if scissors.is_empty() {
            return false;
        }
        // The check lets through only one identical to the one before (#113), not one that differs where we painted.
        g.queue.write_buffer(&self.bounds_uniforms, 0, bytemuck::cast_slice(&[bounds[0] as f32 * scale, bounds[1] as f32 * scale, bounds[2] as f32 * scale, bounds[3] as f32 * scale, d.opacity, if strictly_same { 1.0 } else { 0.0 }, 0.0, 0.0]));
        // The passes go in the same submission as the frame being painted now
        // —it has to be painted again—: a separate one cost one more `submit`,
        // a third of a millisecond per capture.
        self.pending = Some((presented, scissors));
        let new = self.current ^ 1;
        self.group = Some(g.backdrop_group(&self.backgrounds[new].1, self.frost.result()));
        self.ready = true;
        true
    }

    /// The background is `picture` (version `version`), stretched over the
    /// surface and frosted once, in place of a capture of the screen: no
    /// screencopy, no readback, no unmixing. What is under the glass is then
    /// the picture and never a window, but it is bent like any other.
    pub fn set_fixed(&mut self, g: &crate::gpu::Gpu, version: u64, picture: &image::RgbaImage) {
        let (lw, lh) = self.px;
        let mut fitted = image::imageops::resize(picture, lw.max(1), lh.max(1), image::imageops::FilterType::Triangle);
        for p in fitted.pixels_mut() {
            p.0[3] = 255;
        }
        g.queue.write_texture(
            self.backgrounds[self.current].0.as_image_copy(),
            fitted.as_raw(),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(lw.max(1) * 4), rows_per_image: Some(lh.max(1)) },
            wgpu::Extent3d { width: lw.max(1), height: lh.max(1), depth_or_array_layers: 1 },
        );
        // A capture still waiting from before (the lens captured until now) is of the screen, windows and all: not to be unmixed over the picture.
        self.pending = None;
                self.group = Some(g.backdrop_group(&self.backgrounds[self.current].1, self.frost.result()));
        self.bounds = [0, 0, lw as i32, lh as i32];
        self.fixed = Some(version);
        self.fixed_frost = true;
        self.ready = true;
    }

    /// The unmixed background as it stands, once there is one.
    pub fn background(&self) -> Option<&wgpu::TextureView> {
        self.ready.then(|| &self.backgrounds[self.current].1)
    }

    /// Still in its first moments: see `WARM_UP`.
    pub fn warming(&self) -> bool {
        self.fixed.is_none() && self.received < WARM_UP || self.born.elapsed() < WARM_UP_TIME
    }

    /// Unmixes and frosts what the last capture left, in the frame's
    /// submission, before painting it.
    pub fn prepare(&mut self, g: &crate::gpu::Gpu, t: &Pipelines, enc: &mut wgpu::CommandEncoder, scale: f32) {
        if self.fixed.is_some() && (std::mem::take(&mut self.fixed_frost) || self.frosted_with != g.blur_values) {
            self.frosted_with = g.blur_values;
            let (lw, lh) = self.px;
            self.frost.run(g, enc, &self.backgrounds[self.current].1, (0, 0, lw, lh), scale);
            return;
        }
        if self.fixed.is_some() {
            return;
        }
        let Some((presented, scissors)) = self.pending.take() else { return };
        let (Some(capture), (lw, lh)) = (self.capture.as_ref(), self.px) else { return };
        let dev = &g.device;
        // What is outside the box stays as it was: the new background starts
        // out as a copy of the current one, and only the inside is written.
        let new = self.current ^ 1;
        enc.copy_texture_to_texture(self.backgrounds[self.current].0.as_image_copy(), self.backgrounds[new].0.as_image_copy(), wgpu::Extent3d { width: lw, height: lh, depth_or_array_layers: 1 });
        let unmixing = |layout: &wgpu::BindGroupLayout, verdict: Option<&wgpu::Buffer>| {
            let mut entries = vec![
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&capture.1) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&self.canvases[presented].1) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&self.backgrounds[self.current].1) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&t.linear) },
                wgpu::BindGroupEntry { binding: 4, resource: self.bounds_uniforms.as_entire_binding() },
            ];
            if let Some(v) = verdict {
                entries.push(wgpu::BindGroupEntry { binding: 5, resource: v.as_entire_binding() });
            }
            dev.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout, entries: &entries })
        };
        // (`PLEAMAR_LENS_LOG`: the counts of each part, and the canvas, copied to be read back later.)
        let judging = said(|c| c.n).map(|no| {
            let counts = dev.create_buffer(&wgpu::BufferDescriptor { label: Some("verdicts"), size: 8 * scissors.len().max(1) as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            let canvas = (no <= KEPT_PICTURES && LOG.get().and_then(|l| l.as_ref()).is_some_and(|l| l.dir.is_some())).then(|| {
                let row = (lw * 4).div_ceil(256) * 256;
                let b = dev.create_buffer(&wgpu::BufferDescriptor { label: Some("canvas kept"), size: (row * lh) as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
                enc.copy_texture_to_buffer(
                    self.canvases[presented].0.as_image_copy(),
                    wgpu::TexelCopyBufferInfo { buffer: &b, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(lh) } },
                    wgpu::Extent3d { width: lw, height: lh, depth_or_array_layers: 1 },
                );
                (b, row)
            });
            Judging { no, same: self.last_same, parts: scissors.len(), counts, canvas, px: (lw, lh), bgra: matches!(self.canvases[presented].0.format(), wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb) }
        });
        // Every part is unmixed before any is frosted: the frosting of one reads a little way into the next.
        for (k, &scissor) in scissors.iter().enumerate() {
            match &t.checked {
                // Into the draft, counting; then into the new background, or what was there.
                Some((check, settle)) => {
                    enc.clear_buffer(&self.verdict, 0, None);
                    pass(enc, check, &self.draft.1, &unmixing(&check.get_bind_group_layout(0), Some(&self.verdict)), scissor);
                    if let Some(j) = &judging {
                        enc.copy_buffer_to_buffer(&self.verdict, 0, &j.counts, 8 * k as u64, 8);
                    }
                    let settle_group = dev.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: None,
                        layout: &settle.get_bind_group_layout(0),
                        entries: &[
                            wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(&self.draft.1) },
                            wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::TextureView(&self.backgrounds[self.current].1) },
                            wgpu::BindGroupEntry { binding: 8, resource: self.verdict.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 4, resource: self.bounds_uniforms.as_entire_binding() },
                        ],
                    });
                    pass(enc, settle, &self.backgrounds[new].1, &settle_group, scissor);
                }
                None => pass(enc, &t.unmix, &self.backgrounds[new].1, &unmixing(&t.unmix.get_bind_group_layout(0), None), scissor),
            }
        }
        for &scissor in &scissors {
            self.frost.run(g, enc, &self.backgrounds[new].1, scissor, scale);
        }
        self.current = new;
        if t.checked.is_some() {
            self.judging = judging;
        }
    }

    /// `PLEAMAR_LENS_LOG`: what the card counted for the last capture unmixed (it has long been submitted when the
    /// next one arrives): how many of its pixels cannot be our frame plus a background, and so whether it was taken
    /// in or refused; and the canvas it was judged against, as a picture.
    fn read_judging(&mut self, g: &crate::gpu::Gpu) {
        let Some(j) = self.judging.take() else { return };
        j.counts.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        if let Some((b, _)) = &j.canvas {
            b.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        }
        if g.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_millis(200)) }).is_err() {
            return;
        }
        let counts: Vec<u32> = match j.counts.slice(..).get_mapped_range() {
            Ok(view) => bytemuck::cast_slice::<u8, u32>(&view).to_vec(),
            Err(_) => Vec::new(),
        };
        j.counts.unmap();
        let mut parts = Vec::new();
        let mut refused = false;
        for (k, c) in counts.chunks(2).take(j.parts).enumerate() {
            let (seen, wrong) = (c[0], c[1]);
            // The same rule as `settle` (backdrop_check.wgsl), but for a capture taken in once more.
            let no = wrong > 16 && wrong as u64 * 100 > seen as u64;
            refused |= no;
            parts.push(format!("{}{} of {seen} px do not fit our frame ({:.1}%)", if j.parts > 1 { format!("part {}: ", k + 1) } else { String::new() }, wrong, wrong as f64 * 100.0 / seen.max(1) as f64));
        }
        // The same capture a second time is let through whatever it counted (`settle`, #113).
        let refused_now = refused && !j.same;
        said(|c| if refused_now { c.refused += 1 } else { c.kept += 1 });
        let no = j.no;
        let line = format!("lens · capture #{no} judged · {} · {}", parts.join(" · "), if refused_now { "refused" } else if refused { "taken in all the same: it is the one before again, so what does not fit is something drawn over the glass" } else { "taken in" });
        let refused = refused_now;
        if refused { detail_always(no, line) } else { detail(no, || line) }
        if let Some((b, row)) = &j.canvas {
            if let Ok(view) = b.slice(..).get_mapped_range() {
                keep_picture(no, "canvas", j.px.0, j.px.1, *row, &view, j.bgra);
            }
            b.unmap();
        }
    }
}

/// What `PLEAMAR_LENS_LOG` reads back of a capture being unmixed: see `Lens::read_judging`.
struct Judging {
    no: u32,
    same: bool,
    parts: usize,
    counts: wgpu::Buffer,
    canvas: Option<(wgpu::Buffer, u32)>,
    px: (u32, u32),
    bgra: bool,
}

/// `PLEAMAR_LENS_LOG=1`: what the glass is given and what it does with it, said on the console —every capture
/// asked for and how it came back; each one in detail for the first ones, and every one refused after them; and
/// every two seconds, how many of each—. Set to a folder instead of `1`, the first captures are kept there too as
/// pictures, each with the canvas it was judged against (`capture-007.png`, `canvas-007.png`): what the compositor
/// gave and what we had painted. To see from someone's machine why a glass shows what it shows.
struct LensLog {
    dir: Option<std::path::PathBuf>,
    counts: std::sync::Mutex<Counts>,
}

#[derive(Default)]
struct Counts {
    since: Option<std::time::Instant>,
    n: u32,
    asked: u32,
    watching: u32,
    arrived: u32,
    failed: u32,
    unpresented: u32,
    unchanged: u32,
    kept: u32,
    refused: u32,
    said_at: Option<std::time::Instant>,
}

static LOG: std::sync::OnceLock<Option<LensLog>> = std::sync::OnceLock::new();
/// How many captures are said one by one, and kept as pictures, before only the refused ones are.
const DETAILED: u32 = 30;
const KEPT_PICTURES: u32 = 30;

fn lens_log() -> Option<&'static LensLog> {
    LOG.get_or_init(|| {
        let v = std::env::var("PLEAMAR_LENS_LOG").ok().filter(|v| !v.is_empty() && v != "0")?;
        let dir = (v != "1").then(|| std::path::PathBuf::from(&v));
        if let Some(d) = &dir {
            if let Err(e) = std::fs::create_dir_all(d) {
                eprintln!("lens · cannot keep pictures in {}: {e}", d.display());
                return Some(LensLog { dir: None, counts: Default::default() });
            }
            eprintln!("lens · the first {KEPT_PICTURES} captures, and what was painted then, go to {}", d.display());
        }
        Some(LensLog { dir, counts: Default::default() })
    })
    .as_ref()
}

/// With the log on, counts something (and says the counts every two seconds); `None` with it off.
fn said<T>(f: impl FnOnce(&mut Counts) -> T) -> Option<T> {
    let log = lens_log()?;
    let mut c = log.counts.lock().unwrap();
    let now = std::time::Instant::now();
    c.since.get_or_insert(now);
    let out = f(&mut c);
    if c.said_at.is_none_or(|t| now - t >= std::time::Duration::from_secs(2)) && c.since.is_some_and(|s| now - s >= std::time::Duration::from_millis(500)) {
        let secs = c.since.map_or(0.0, |s| (now - s).as_secs_f32()).max(0.1);
        eprintln!(
            "lens · in {secs:.1} s · asked {} ({} watching for a change) · arrived {} · failed {} · before anything was presented {} · nothing new {} · taken in {} · refused {}",
            c.asked, c.watching, c.arrived, c.failed, c.unpresented, c.unchanged, c.kept, c.refused
        );
        let n = c.n;
        *c = Counts { n, said_at: Some(now), since: Some(now), ..Default::default() };
    }
    Some(out)
}

/// What a still sheet does about what is behind it (`Pace::still`), said when it changes.
pub fn log_still(sheet: impl std::fmt::Debug, what: &str) {
    if lens_log().is_none() {
        return;
    }
    static LAST: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
    let now = format!("{sheet:?} {what}");
    let mut last = LAST.lock().unwrap();
    if *last != now {
        eprintln!("lens · still · {now}");
        *last = now;
    }
}

/// A capture that could not be asked for, and why (said when the reason changes).
pub fn log_not_asked(sheet: impl std::fmt::Debug, why: &str) {
    if lens_log().is_none() {
        return;
    }
    static LAST: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
    let now = format!("{sheet:?}: {why}");
    let mut last = LAST.lock().unwrap();
    if *last != now {
        eprintln!("lens · not asked · {now}");
        *last = now;
    }
}

/// A capture asked for (`Sheet::request_backdrop`): counted, with the log on.
pub fn log_asked(watching: bool) {
    said(|c| {
        c.asked += 1;
        if watching {
            c.watching += 1;
        }
    });
}

fn detail(no: u32, line: impl FnOnce() -> String) {
    if no <= DETAILED {
        eprintln!("{}", line());
    }
}

/// Said even past the first ones (a refused capture), at most once a second.
fn detail_always(no: u32, line: String) {
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    if no <= DETAILED || last.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(1)) {
        *last = Some(std::time::Instant::now());
        eprintln!("{line}");
    }
}

/// One of the first captures (or the canvas it was judged against), as a picture in the log's folder.
fn keep_picture(no: u32, what: &str, w: u32, h: u32, stride: u32, data: &[u8], bgra: bool) {
    let Some(dir) = lens_log().and_then(|l| l.dir.as_ref()) else { return };
    if no > KEPT_PICTURES || w == 0 || h == 0 {
        return;
    }
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let Some(row) = data.get((y * stride) as usize..(y * stride + w * 4) as usize) else { return };
        for p in row.chunks_exact(4) {
            // Opaque, to be looked at: what is behind has no transparency of its own.
            if bgra {
                rgba.extend_from_slice(&[p[2], p[1], p[0], 255]);
            } else {
                rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
    }
    let path = dir.join(format!("{what}-{no:03}.png"));
    if let Err(e) = image::save_buffer(&path, &rgba, w, h, image::ExtendedColorType::Rgba8) {
        eprintln!("lens · {}: {e}", path.display());
    }
}

/// One full-screen triangle with `pipeline`, clipped to `scissor`, onto what is there.
fn pass(enc: &mut wgpu::CommandEncoder, pipeline: &wgpu::RenderPipeline, target: &wgpu::TextureView, group: &wgpu::BindGroup, scissor: (u32, u32, u32, u32)) {
    let mut p = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    p.set_pipeline(pipeline);
    p.set_bind_group(0, group, &[]);
    p.set_scissor_rect(scissor.0, scissor.1, scissor.2, scissor.3);
    p.draw(0..3, 0..1);
}

/// The frosting's pictures, one per pass of the scene's blur (`blur.rs`),
/// sized for it; the last is what the shapes shader reads.
struct Frost {
    levels: Vec<(wgpu::Texture, wgpu::TextureView)>,
    uniforms: Vec<wgpu::Buffer>,
    /// The plan it was made for: one made for another is made again.
    generation: u64,
}

impl Frost {
    fn new(d: &wgpu::Device, px: (u32, u32), b: &crate::blur::Built) -> Frost {
        use wgpu::TextureUsages as U;
        let mut size = (px.0 as f32, px.1 as f32);
        let mut levels = Vec::new();
        for step in &b.plan.steps {
            size = (size.0 * step.scale, size.1 * step.scale);
            let px = ((size.0.ceil() as u32).clamp(1, 8192), (size.1.ceil() as u32).clamp(1, 8192));
            levels.push(texture(d, "frost", px, crate::blur::FORMAT, U::RENDER_ATTACHMENT | U::TEXTURE_BINDING));
        }
        let uniforms = b.plan.steps.iter().map(|_| d.create_buffer(&wgpu::BufferDescriptor { label: Some("frost"), size: 64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })).collect();
        Frost { levels, uniforms, generation: b.generation }
    }

    fn result(&self) -> &wgpu::TextureView {
        &self.levels[self.levels.len() - 1].1
    }

    /// Frosts the sharp background in `scissor` (real pixels) through the
    /// blur's passes. Every pass is clipped to the scissor at its size, with
    /// the reach of the frosting around it.
    fn run(&self, g: &crate::gpu::Gpu, enc: &mut wgpu::CommandEncoder, sharp: &wgpu::TextureView, scissor: (u32, u32, u32, u32), scale: f32) {
        let b = g.blur();
        if b.generation != self.generation {
            return;
        }
        let dev = &g.device;
        let values = g.blur_values;
        let full = (sharp.texture().width() as f32, sharp.texture().height() as f32);
        let reach = b.plan.reach * scale.max(1.0);
        // The pictures kept for later passes, by slot; an unused one reads the backdrop.
        let mut kept: [&wgpu::TextureView; 4] = [sharp; 4];
        let mut prev = sharp;
        let mut prev_size = full;
        for (k, step) in b.plan.steps.iter().enumerate() {
            let (w, h) = (self.levels[k].0.width(), self.levels[k].0.height());
            let f = w as f32 / full.0;
            let m = (reach * f).ceil() as u32 + 2;
            let x0 = ((scissor.0 as f32 * f) as u32).saturating_sub(m);
            let y0 = ((scissor.1 as f32 * f) as u32).saturating_sub(m);
            let x1 = (((scissor.0 + scissor.2) as f32 * f).ceil() as u32 + m).min(w);
            let y1 = (((scissor.1 + scissor.3) as f32 * f).ceil() as u32 + m).min(h);
            let clip = (x0.min(w.saturating_sub(1)), y0.min(h.saturating_sub(1)), x1.saturating_sub(x0).max(1), y1.saturating_sub(y0).max(1));
            let u = [w as f32, h as f32, prev_size.0, prev_size.1, scale, step.index as f32, step.count as f32, k as f32, values[0], values[1], values[2], values[3], values[4], values[5], values[6], values[7]];
            g.queue.write_buffer(&self.uniforms[k], 0, bytemuck::cast_slice(&u));
            let group = dev.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &b.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(prev) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(sharp) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(kept[0]) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(kept[1]) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(kept[2]) },
                    wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(kept[3]) },
                    wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(&g.blur_sampler) },
                    wgpu::BindGroupEntry { binding: 7, resource: self.uniforms[k].as_entire_binding() },
                ],
            });
            pass(enc, b.pipeline(&step.entry), &self.levels[k].1, &group, clip);
            if let Some(slot) = step.keep {
                kept[slot as usize] = &self.levels[k].1;
            }
            prev = &self.levels[k].1;
            prev_size = (w as f32, h as f32);
        }
    }
}

/// The lens of a scene that is the whole screen —pleamar-wm's—: there is no
/// compositor to ask what is behind, because what is behind is what the scene
/// painted itself before the glass (the wallpaper, the windows). Right before
/// a glass is painted, that is copied and frosted, and the glass bends it like
/// any other background.
///
/// A scene on a surface uses it too, when a glass of its has something of the
/// scene's own under it: then what is behind that glass is what the scene
/// painted before it, over whatever the capture says is behind the surface.
pub struct Own {
    pub px: (u32, u32),
    sharp: (wgpu::Texture, wgpu::TextureView),
    frost: Frost,
    /// What the shapes shader reads.
    pub group: wgpu::BindGroup,
}

impl Own {
    /// The blur plan its frosting was made for.
    pub fn blur_generation(&self) -> u64 {
        self.frost.generation
    }

    pub fn new(g: &crate::gpu::Gpu, px: (u32, u32)) -> Own {
        use wgpu::TextureUsages as U;
        let d = &g.device;
        let sharp = texture(d, "painted below the glass", px, wgpu::TextureFormat::Rgba8Unorm, U::RENDER_ATTACHMENT | U::TEXTURE_BINDING);
        let frost = Frost::new(d, px, g.blur());
        let group = g.backdrop_group(&sharp.1, frost.result());
        Own { px, sharp, frost, group }
    }

    /// What has been painted so far in `painted`, in `scissor` (real pixels),
    /// becomes what is behind the glass; with `behind` —the unmixed background
    /// of a surface—, over that.
    pub fn take(&self, g: &crate::gpu::Gpu, t: &Pipelines, enc: &mut wgpu::CommandEncoder, painted: &wgpu::TextureView, behind: Option<&wgpu::TextureView>, scissor: (u32, u32, u32, u32), scale: f32) {
        let pipeline = if behind.is_some() { &t.own_over } else { &t.own };
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(painted) }];
        if let Some(behind) = behind {
            entries.push(wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(behind) });
        }
        let group = g.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &pipeline.get_bind_group_layout(0), entries: &entries });
        pass(enc, pipeline, &self.sharp.1, &group, scissor);
        self.frost.run(g, enc, &self.sharp.1, scissor, scale);
    }
}

/// At most, one capture of what is behind every so often, of either kind: the
/// one asked after presenting and the one that watches for a change behind.
/// A scene sets its own with `lens every 250ms`.
pub const CAPTURE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// The least and the most a scene's `lens every` can say: below a refresh it
/// would be no limit at all, and past a minute the glass is as good as frozen
/// (`lens hold while` says that).
pub const INTERVAL_RANGE: (f32, f32) = (0.016, 60.0);

/// The interval between captures, from what `lens every` gave this round (in
/// seconds), or the default without one.
pub fn capture_interval(seconds: Option<f32>) -> std::time::Duration {
    match seconds {
        Some(s) if s.is_finite() => std::time::Duration::from_secs_f32(s.clamp(INTERVAL_RANGE.0, INTERVAL_RANGE.1)),
        _ => CAPTURE_INTERVAL,
    }
}

/// How a sheet with a lens goes about its captures this round. `since` is the
/// time since its last capture (asked after presenting, or arrived while
/// watching), `held` what `lens hold while` says, `warming` whether it is in
/// its first moments (`WARM_UP`, which nothing holds back).
#[derive(Clone, Copy, Debug)]
pub struct Pace {
    pub interval: std::time::Duration,
    pub held: bool,
    pub warming: bool,
    pub since: std::time::Duration,
}

/// How long a watch (a capture that arrives when something changes behind) is waited for before a plain
/// capture is asked instead. Some compositors never answer one —Hyprland 0.56, with a window moving right
/// under the glass (#118)—, and the glass kept what was behind it before, until something made it paint.
pub const WATCH_PATIENCE: std::time::Duration = std::time::Duration::from_millis(600);

/// The compositor did not say a change behind the glass that a plain capture then found: from then on the
/// glass does not wait to be told, it takes a plain capture each interval (which the compositor answers the
/// next time it paints the monitor: with nothing moving there, none).
static WATCH_DEAF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How often a glass that is not told of changes looks behind it while it is still.
pub const DEAF_EVERY: std::time::Duration = std::time::Duration::from_millis(250);

pub fn watch_deaf() -> bool {
    WATCH_DEAF.load(std::sync::atomic::Ordering::Relaxed)
}

/// A watch was answered with something new: this compositor says when what is behind changes, and an
/// unanswered watch is only a still background (no plain capture stands in for it any more).
static WATCH_HEARD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn watch_heard() -> bool {
    WATCH_HEARD.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn watch_came(new: bool) {
    if new {
        WATCH_HEARD.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A plain capture that stood in for an unanswered watch came back: if it brought something new, the
/// compositor does not tell changes (see `WATCH_DEAF`).
pub fn watch_replaced_came(new: bool) {
    if new && !WATCH_DEAF.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!("render · the compositor does not say when what is behind the glass changes: the glass captures it every interval instead");
    }
}

/// How long a capture asked after presenting is waited for before it is taken for lost and asked again: the
/// compositor answers it the next time it paints the monitor, which presenting makes it do: within a frame or two.
/// Each one lost in a row doubles it, up to 4 s: a monitor that is off answers none.
pub const CAPTURE_LOST: std::time::Duration = std::time::Duration::from_millis(500);

pub fn capture_lost_after(lost: u32) -> std::time::Duration {
    CAPTURE_LOST * (1u32 << lost.min(3))
}

/// What a sheet that has not painted this round does about what is behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Still {
    /// Nothing: held, or a capture is already on the way.
    Nothing,
    /// Too soon after the last capture: it waits this much more (an
    /// appointment wakes the render for it).
    Wait(std::time::Duration),
    /// A capture now, without waiting for a change: one it owes, because it
    /// painted —or was held— while it could not capture.
    Capture,
    /// Watch: a capture as soon as something changes behind.
    Watch,
}

impl Pace {
    /// After painting: whether to ask the capture of what was just presented.
    /// A nudge is a paint made only to have one answered: it always asks.
    pub fn after_paint(&self, nudged: bool) -> bool {
        self.warming || nudged || (!self.held && self.since >= self.interval)
    }

    /// Not painted this round, with no capture on the way.
    pub fn still(&self, owed: bool) -> Still {
        if self.warming {
            return Still::Watch;
        }
        if self.held {
            return Still::Nothing;
        }
        if self.since < self.interval {
            return Still::Wait(self.interval - self.since);
        }
        if owed { Still::Capture } else { Still::Watch }
    }
}

/// The size of the squares the parts of a capture are made of, in logical pixels: the same 16 the box is rounded to.
const PART_TILE: i32 = 16;
/// More parts than this, or parts that are most of the box anyway, are not worth the extra requests: the box whole.
const MAX_PARTS: usize = 12;
/// Past this many before joining (a few hundred rows of a curve), it is not a shape worth cutting up.
const MAX_RAW_PARTS: usize = 600;

/// The boxes of glass (`[x0, y0, x1, y1]`, logical, in a surface of `w` x `h`) and, with the room `margin`
/// leaves them for the frosting, the few rectangles (`[x, y, width, height]`) that hold all of them and
/// little else. A shell's frame is a ring round the screen: its box is the whole screen, but four bands
/// hold it. Empty when the box whole is as good: one part, many parts, or parts that are most of it.
pub fn capture_parts(boxes: &[[i32; 4]], margin: i32, w: i32, h: i32) -> Vec<[i32; 4]> {
    if boxes.len() < 2 || w <= 0 || h <= 0 {
        return Vec::new();
    }
    let (cols, rows) = ((w + PART_TILE - 1) / PART_TILE, (h + PART_TILE - 1) / PART_TILE);
    let mut marked = vec![false; (cols * rows) as usize];
    for b in boxes {
        let (c0, c1) = (((b[0] - margin).max(0)) / PART_TILE, ((b[2] + margin + PART_TILE - 1) / PART_TILE).min(cols));
        let (r0, r1) = (((b[1] - margin).max(0)) / PART_TILE, ((b[3] + margin + PART_TILE - 1) / PART_TILE).min(rows));
        for r in r0..r1 {
            for c in c0..c1 {
                marked[(r * cols + c) as usize] = true;
            }
        }
    }
    // Runs of squares along each row; a run that is the same as the one above it is the same rectangle, taller.
    let mut parts: Vec<[i32; 4]> = Vec::new();
    let mut open: Vec<(i32, i32, i32)> = Vec::new();
    let close = |parts: &mut Vec<[i32; 4]>, (c0, c1, r0): (i32, i32, i32), r1: i32| {
        let (x, y) = (c0 * PART_TILE, r0 * PART_TILE);
        parts.push([x, y, (c1 * PART_TILE).min(w) - x, (r1 * PART_TILE).min(h) - y]);
    };
    for r in 0..=rows {
        let mut runs: Vec<(i32, i32)> = Vec::new();
        if r < rows {
            let mut c = 0;
            while c < cols {
                if marked[(r * cols + c) as usize] {
                    let c0 = c;
                    while c < cols && marked[(r * cols + c) as usize] {
                        c += 1;
                    }
                    runs.push((c0, c));
                } else {
                    c += 1;
                }
            }
        }
        let mut next = Vec::new();
        for o in open.drain(..) {
            if runs.contains(&(o.0, o.1)) {
                next.push(o);
            } else {
                close(&mut parts, o, r);
            }
        }
        for run in runs {
            if !next.iter().any(|o| (o.0, o.1) == run) {
                next.push((run.0, run.1, r));
            }
        }
        open = next;
        if parts.len() > MAX_RAW_PARTS {
            return Vec::new();
        }
    }
    let area = |r: &[i32; 4]| r[2] as i64 * r[3] as i64;
    // A curved edge changes its run of squares on every row, which makes a part per row: the two parts that grow
    // least when joined are joined until few are left (a frame's rounded corners end up in its bands).
    let join = |a: &[i32; 4], b: &[i32; 4]| {
        let (x0, y0) = (a[0].min(b[0]), a[1].min(b[1]));
        [x0, y0, (a[0] + a[2]).max(b[0] + b[2]) - x0, (a[1] + a[3]).max(b[1] + b[3]) - y0]
    };
    while parts.len() > MAX_PARTS {
        let mut best: Option<(i64, usize, usize)> = None;
        for i in 0..parts.len() {
            for j in i + 1..parts.len() {
                let cost = area(&join(&parts[i], &parts[j])) - area(&parts[i]) - area(&parts[j]);
                if best.is_none_or(|b| cost < b.0) {
                    best = Some((cost, i, j));
                }
            }
        }
        let Some((_, i, j)) = best else { break };
        parts[i] = join(&parts[i], &parts[j]);
        parts.swap_remove(j);
        // What the joined part now holds whole is not taken twice.
        let k = parts[i];
        let inside = |r: &[i32; 4]| r[0] >= k[0] && r[1] >= k[1] && r[0] + r[2] <= k[0] + k[2] && r[1] + r[3] <= k[1] + k[3];
        parts.retain(|r| *r == k || !inside(r));
    }
    let (all, hull) = (parts.iter().map(area).sum::<i64>(), {
        let (x0, y0) = (parts.iter().map(|r| r[0]).min().unwrap_or(0), parts.iter().map(|r| r[1]).min().unwrap_or(0));
        let (x1, y1) = (parts.iter().map(|r| r[0] + r[2]).max().unwrap_or(0), parts.iter().map(|r| r[1] + r[3]).max().unwrap_or(0));
        (x1 - x0) as i64 * (y1 - y0) as i64
    });
    if parts.len() < 2 || parts.len() > MAX_PARTS || all * 10 > hull * 7 {
        return Vec::new();
    }
    parts
}

#[cfg(test)]
mod parts_tests {
    use super::capture_parts;

    #[test]
    fn a_ring_round_the_screen_is_four_bands_not_the_screen() {
        // A frame: a band along each edge, ten pixels thick.
        let boxes = [[0, 0, 1920, 10], [0, 0, 10, 1080], [1910, 0, 1920, 1080], [0, 1070, 1920, 1080]];
        let parts = capture_parts(&boxes, 42, 1920, 1080);
        assert!(parts.len() >= 3 && parts.len() <= 12, "{parts:?}");
        let area: i64 = parts.iter().map(|r| r[2] as i64 * r[3] as i64).sum();
        assert!(area < 1920 * 1080 / 2, "{area}");
        // Everything that must be captured is inside some part.
        for b in &boxes {
            for (x, y) in [(b[0], b[1]), (b[2] - 1, b[3] - 1), (b[0], b[3] - 1), (b[2] - 1, b[1])] {
                assert!(parts.iter().any(|r| x >= r[0] && x < r[0] + r[2] && y >= r[1] && y < r[1] + r[3]), "({x}, {y}) is in no part: {parts:?}");
            }
        }
    }

    #[test]
    fn one_box_or_boxes_that_fill_their_hull_stay_whole() {
        assert!(capture_parts(&[[100, 100, 400, 200]], 42, 1920, 1080).is_empty());
        assert!(capture_parts(&[[0, 0, 1000, 1000], [500, 0, 1920, 1080]], 42, 1920, 1080).is_empty());
    }

    #[test]
    fn two_far_apart_boxes_are_two_parts() {
        let parts = capture_parts(&[[0, 0, 200, 40], [1700, 1000, 1920, 1080]], 42, 1920, 1080);
        assert_eq!(parts.len(), 2, "{parts:?}");
    }
}

/// How much margin the frosting needs around a glass, in logical pixels:
/// the reach of the scene's blur.
pub fn margin() -> f32 {
    crate::blur::reach() + 2.0
}

/// `lens = image "file"`: the glass bends this picture (the wallpaper, as big as
/// the screen or smaller) instead of a capture of what is behind. The file is
/// looked at again twice a second at most, and when it changes the glass is
/// remade. Its version goes up each time it is read.
pub struct FixedSource {
    path: std::path::PathBuf,
    stamp: Option<(std::time::SystemTime, u64)>,
    looked: std::time::Instant,
    version: u64,
    picture: Option<std::sync::Arc<image::RgbaImage>>,
}

impl FixedSource {
    /// `first` is the version before the first read: a new source (another file) must not
    /// repeat the numbers of the one before, or a lens would think it already has the picture.
    pub fn new(path: std::path::PathBuf, first: u64) -> FixedSource {
        FixedSource { path, stamp: None, looked: std::time::Instant::now() - std::time::Duration::from_secs(60), version: first, picture: None }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn poll(&mut self) -> Option<(u64, std::sync::Arc<image::RgbaImage>)> {
        if self.looked.elapsed() >= std::time::Duration::from_millis(500) {
            self.looked = std::time::Instant::now();
            let stamp = std::fs::metadata(&self.path).ok().and_then(|m| Some((m.modified().ok()?, m.len())));
            if stamp.is_some() && stamp != self.stamp {
                // A half-written file fails to decode: it is tried again next time.
                if let Ok(img) = image::open(&self.path) {
                    self.picture = Some(std::sync::Arc::new(img.to_rgba8()));
                    self.stamp = stamp;
                    self.version += 1;
                }
            }
        }
        Some((self.version, self.picture.clone()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// What is behind the glass changes on every refresh (a terminal scrolling
    /// under it): rounds a refresh apart, each capture answered in the next one,
    /// each one showing something new and so painted. Returns how many captures
    /// arrived in ten seconds.
    fn scrolling(interval: Duration) -> u32 {
        #[derive(PartialEq)]
        enum Asked {
            Idle,
            AfterPresent,
            Watching,
        }
        let refresh = Duration::from_micros(16_667);
        let (mut now, mut taken, mut owed, mut asked, mut to_paint, mut arrived) = (Duration::ZERO, None::<Duration>, false, Asked::Idle, true, 0);
        let since = |now: Duration, taken: Option<Duration>| taken.map_or(Duration::from_secs(3600), |t| now - t);
        while now < Duration::from_secs(10) {
            // What was asked last round arrives: there is always something new.
            if asked != Asked::Idle {
                if asked == Asked::Watching {
                    taken = Some(now);
                }
                asked = Asked::Idle;
                arrived += 1;
                to_paint = true;
            }
            let pace = Pace { interval, held: false, warming: false, since: since(now, taken) };
            if std::mem::take(&mut to_paint) {
                if pace.after_paint(false) {
                    (asked, taken, owed) = (Asked::AfterPresent, Some(now), false);
                } else {
                    owed = true;
                }
            } else {
                match pace.still(owed) {
                    Still::Capture => (asked, taken, owed) = (Asked::AfterPresent, Some(now), false),
                    Still::Watch => asked = Asked::Watching,
                    Still::Wait(left) => assert!(left <= interval),
                    Still::Nothing => {}
                }
            }
            now += refresh;
        }
        arrived
    }

    #[test]
    fn captures_keep_to_the_interval_watching_too() {
        // Before issue 106 a paint and a watch each cycle made ~38 a second at 50 ms.
        for (ms, most) in [(50, 201), (100, 101), (250, 41), (1000, 11)] {
            let n = scrolling(Duration::from_millis(ms));
            assert!(n <= most, "{ms} ms: {n} captures in 10 s, at most {most}");
            assert!(n * 2 >= most, "{ms} ms: {n} captures in 10 s: the glass should still follow");
        }
    }

    #[test]
    fn still_it_watches_and_owed_it_captures() {
        let p = Pace { interval: CAPTURE_INTERVAL, held: false, warming: false, since: Duration::from_secs(1) };
        assert_eq!(p.still(false), Still::Watch);
        assert_eq!(p.still(true), Still::Capture);
        let soon = Pace { since: Duration::from_millis(20), ..p };
        assert_eq!(soon.still(false), Still::Wait(Duration::from_millis(30)));
        assert!(!soon.after_paint(false));
        assert!(soon.after_paint(true), "a nudge always asks");
    }

    #[test]
    fn held_nothing_is_captured_but_warming_is_not_held() {
        let p = Pace { interval: CAPTURE_INTERVAL, held: true, warming: false, since: Duration::from_secs(5) };
        assert_eq!(p.still(true), Still::Nothing);
        assert!(!p.after_paint(false));
        let warming = Pace { warming: true, ..p };
        assert!(warming.after_paint(false));
        assert_eq!(warming.still(false), Still::Watch);
        // Let go, with what it owes: one capture straight away.
        assert_eq!(Pace { held: false, ..p }.still(true), Still::Capture);
    }

    #[test]
    fn a_lost_capture_is_asked_again_less_and_less_often() {
        assert_eq!(capture_lost_after(0), Duration::from_millis(500));
        assert_eq!(capture_lost_after(1), Duration::from_secs(1));
        assert_eq!(capture_lost_after(3), Duration::from_secs(4));
        assert_eq!(capture_lost_after(40), Duration::from_secs(4));
    }

    #[test]
    fn the_scene_interval_is_kept_in_range() {
        assert_eq!(capture_interval(None), CAPTURE_INTERVAL);
        assert_eq!(capture_interval(Some(f32::NAN)), CAPTURE_INTERVAL);
        assert_eq!(capture_interval(Some(0.25)), Duration::from_secs_f32(0.25));
        assert_eq!(capture_interval(Some(0.0)), Duration::from_secs_f32(INTERVAL_RANGE.0));
        assert_eq!(capture_interval(Some(1e6)), Duration::from_secs_f32(INTERVAL_RANGE.1));
    }
}

/// What the scene painted, added to what it had: a few boxes, or the box round them all once there are many
/// (a ripple that goes round the glass ends up as all of it, and then the capture is compared whole).
fn join_ours(had: Option<Vec<[f32; 4]>>, pieces: Option<&[[f32; 4]]>) -> Option<Vec<[f32; 4]>> {
    let (mut had, pieces) = (had?, pieces?);
    had.extend(pieces.iter().filter(|b| b[2] > b[0] && b[3] > b[1]));
    if had.len() > 16 {
        let u = had.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |u, b| [u[0].min(b[0]), u[1].min(b[1]), u[2].max(b[2]), u[3].max(b[3])]);
        had = vec![u];
    }
    Some(had)
}

/// Whether a capture on the card is the same as the last one taken in, as the processor judges one in
/// shared memory: one pixel in nine, no colour off by more than three levels; or the same but where the
/// scene painted itself (`ours`), when that leaves more than half to tell by (#125). The card counts and
/// only the counts (twelve bytes) are read back, so the capture, megabytes of it, never leaves the card.
struct Compare {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    count: wgpu::Buffer,
}

const COMPARE_SHADER: &str = r#"
@group(0) @binding(0) var fresh: texture_2d<f32>;
@group(0) @binding(1) var kept: texture_2d<f32>;
// [differ anywhere, differ outside ours, looked at outside ours]
@group(0) @binding(2) var<storage, read_write> n: array<atomic<u32>, 3>;
// Ours, [x0, y0, x1, y1] in the capture's pixels.
@group(0) @binding(3) var<storage, read> ours: array<vec4<i32>>;
@compute @workgroup_size(16, 16) fn main(@builtin(global_invocation_id) g: vec3<u32>) {
    let d = textureDimensions(fresh);
    let p = g.xy * 3u;
    if p.x >= d.x || p.y >= d.y {
        return;
    }
    let q = vec2<i32>(p);
    var theirs = true;
    for (var k = 0u; k < arrayLength(&ours); k++) {
        let r = ours[k];
        if q.x >= r.x && q.x < r.z && q.y >= r.y && q.y < r.w {
            theirs = false;
            break;
        }
    }
    let a = textureLoad(fresh, q, 0).rgb;
    let b = textureLoad(kept, q, 0).rgb;
    let m = max(abs(a.r - b.r), max(abs(a.g - b.g), abs(a.b - b.b)));
    let differs = m > 3.5 / 255.0;
    if differs {
        atomicAdd(&n[0], 1u);
    }
    if theirs {
        atomicAdd(&n[2], 1u);
        if differs {
            atomicAdd(&n[1], 1u);
        }
    }
}
"#;

impl Compare {
    fn new(d: &wgpu::Device) -> Compare {
        let tex = |binding| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None };
        let buf = |binding, read_only, min| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: wgpu::BufferSize::new(min) }, count: None };
        let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("a capture compared"), entries: &[tex(0), tex(1), buf(2, false, 12), buf(3, true, 16)] });
        let pl = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("a capture compared"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let shader = d.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("a capture compared"), source: wgpu::ShaderSource::Wgsl(COMPARE_SHADER.into()) });
        let pipeline = d.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some("a capture compared"), layout: Some(&pl), module: &shader, entry_point: Some("main"), compilation_options: Default::default(), cache: None });
        let count = d.create_buffer(&wgpu::BufferDescriptor { label: Some("pixels that differ"), size: 12, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        Compare { pipeline, layout, count }
    }

    /// (Strictly the same, the same where it is not ours): `false` if they differ, or if the card could not say in time.
    fn same(&self, g: &crate::gpu::Gpu, fresh: &wgpu::Texture, kept: &wgpu::Texture, size: (u32, u32), ours: &[[i64; 4]]) -> (bool, bool) {
        use wgpu::util::DeviceExt;
        let (dev, queue) = (&g.device, &g.queue);
        let clamp = |v: i64| v.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        // Never empty: a box with nothing in it stands for none.
        let mut rects: Vec<i32> = ours.iter().flat_map(|r| r.map(clamp)).collect();
        if rects.is_empty() {
            rects = vec![0; 4];
        }
        let ours_buf = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("ours, not compared"), contents: bytemuck::cast_slice(&rects), usage: wgpu::BufferUsages::STORAGE });
        let (fv, kv) = (fresh.create_view(&Default::default()), kept.create_view(&Default::default()));
        let group = dev.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&fv) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&kv) },
                wgpu::BindGroupEntry { binding: 2, resource: self.count.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: ours_buf.as_entire_binding() },
            ],
        });
        queue.write_buffer(&self.count, 0, &[0; 12]);
        let mut enc = dev.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("a capture compared") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("a capture compared"), timestamp_writes: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(size.0.div_ceil(3).div_ceil(16), size.1.div_ceil(3).div_ceil(16), 1);
        }
        // A new one each time: one that was not mapped in time would still be waiting.
        let read = dev.create_buffer(&wgpu::BufferDescriptor { label: Some("pixels that differ, to read"), size: 12, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        enc.copy_buffer_to_buffer(&self.count, 0, &read, 0, 12);
        let index = queue.submit([enc.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        read.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r.is_ok());
        });
        let waited = dev.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(std::time::Duration::from_millis(50)) });
        if !(waited.is_ok() && rx.recv_timeout(std::time::Duration::from_millis(50)).unwrap_or(false)) {
            return (false, false);
        }
        let Ok(view) = read.slice(..).get_mapped_range() else { return (false, false) };
        let n: [u32; 3] = bytemuck::cast_slice::<u8, u32>(&view[..12]).try_into().unwrap();
        drop(view);
        read.unmap();
        let looked = size.0.div_ceil(3) as u64 * size.1.div_ceil(3) as u64;
        let strictly = n[0] == 0;
        (strictly, strictly || (!ours.is_empty() && n[2] as u64 * 2 > looked && n[1] == 0))
    }
}
