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
    frost: Frost,
    /// A sample of the last capture, and of which box it was: so as not to
    /// repeat the work if nothing has changed.
    sample: Vec<u8>,
    /// The card's own look at a capture it holds (`receive`), made the first time one arrives.
    compare: Option<Compare>,
    pub bounds: [i32; 4],
    bounds_uniforms: wgpu::Buffer,
    /// A received capture whose background is still to be unmixed: which
    /// canvas was presented and with which clip.
    pending: Option<(usize, (u32, u32, u32, u32))>,
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
            verdict: d.create_buffer(&wgpu::BufferDescriptor { label: Some("verdict"), size: 8, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }),
            frost: Frost::new(d, px, b),
            sample: Vec::new(),
            compare: None,
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
        let Some(presented) = self.presented else { return false };
        // A capture without data is one that failed.
        let size = (d.width, d.height);
        let card = d.card.clone();
        if card.is_none() && d.data.is_none() {
            return false;
        }
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
        // A capture the compositor wrote on the card is looked at there, one pixel
        // in nine, by the card (`Compare`); the processor never reads it.
        let dev = &g.device;
        let same_place = self.ready && !self.warming() && bounds == self.bounds;
        let same_size = self.capture.as_ref().is_some_and(|f| f.2 == size);
        if let Some(card) = &card {
            if same_place && same_size && !self.sample.is_empty() && self.sample.len() == 1 && self.sample[0] == 1 {
                let old = &self.capture.as_ref().unwrap().0;
                let compare = self.compare.get_or_insert_with(|| Compare::new(dev));
                if !compare.differs(g, &card.texture, old, size) {
                    return false;
                }
            }
            self.sample = vec![1];
            self.bounds = bounds;
            if !same_size {
                let (tx, v) = texture(dev, "capture", size, wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST);
                self.capture = Some((tx, v, size));
            }
            let capture = self.capture.as_ref().unwrap();
            let mut enc = dev.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("a capture copied on the card") });
            enc.copy_texture_to_texture(card.texture.as_image_copy(), capture.0.as_image_copy(), wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 });
            g.queue.submit([enc.finish()]);
        } else {
            let data: &[u8] = (**d.data.as_ref().unwrap()).as_ref();
            let sample: Vec<u8> = data.iter().copied().step_by(13).collect();
            if same_place && sample.len() == self.sample.len() && sample.iter().zip(&self.sample).all(|(a, b)| a.abs_diff(*b) <= 3) {
                return false;
            }
            self.sample = sample;
            self.bounds = bounds;
            if !same_size {
                let (tx, v) = texture(dev, "capture", size, wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST);
                self.capture = Some((tx, v, size));
            }
            let capture = self.capture.as_ref().unwrap();
            g.queue.write_texture(
                capture.0.as_image_copy(),
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(d.stride), rows_per_image: Some(d.height) },
                wgpu::Extent3d { width: d.width, height: d.height, depth_or_array_layers: 1 },
            );
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
        let scissor = (x0, y0, x1 - x0, y1 - y0);
        g.queue.write_buffer(&self.bounds_uniforms, 0, bytemuck::cast_slice(&[bounds[0] as f32 * scale, bounds[1] as f32 * scale, bounds[2] as f32 * scale, bounds[3] as f32 * scale, d.opacity, 0.0, 0.0, 0.0]));
        // The passes go in the same submission as the frame being painted now
        // —it has to be painted again—: a separate one cost one more `submit`,
        // a third of a millisecond per capture.
        self.pending = Some((presented, scissor));
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
        let Some((presented, scissor)) = self.pending.take() else { return };
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
        match &t.checked {
            // Into the draft, counting; then into the new background, or what was there.
            Some((check, settle)) => {
                enc.clear_buffer(&self.verdict, 0, None);
                pass(enc, check, &self.draft.1, &unmixing(&check.get_bind_group_layout(0), Some(&self.verdict)), scissor);
                let settle_group = dev.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &settle.get_bind_group_layout(0),
                    entries: &[
                        wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(&self.draft.1) },
                        wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::TextureView(&self.backgrounds[self.current].1) },
                        wgpu::BindGroupEntry { binding: 8, resource: self.verdict.as_entire_binding() },
                    ],
                });
                pass(enc, settle, &self.backgrounds[new].1, &settle_group, scissor);
            }
            None => pass(enc, &t.unmix, &self.backgrounds[new].1, &unmixing(&t.unmix.get_bind_group_layout(0), None), scissor),
        }
        self.frost.run(g, enc, &self.backgrounds[new].1, scissor, scale);
        self.current = new;
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
    fn the_scene_interval_is_kept_in_range() {
        assert_eq!(capture_interval(None), CAPTURE_INTERVAL);
        assert_eq!(capture_interval(Some(f32::NAN)), CAPTURE_INTERVAL);
        assert_eq!(capture_interval(Some(0.25)), Duration::from_secs_f32(0.25));
        assert_eq!(capture_interval(Some(0.0)), Duration::from_secs_f32(INTERVAL_RANGE.0));
        assert_eq!(capture_interval(Some(1e6)), Duration::from_secs_f32(INTERVAL_RANGE.1));
    }
}


/// Whether a capture on the card differs from the last one taken in, as the old
/// way did on the processor: one pixel in nine, any colour by more than three
/// levels. The card counts them and only that count (four bytes) is read back,
/// so the capture, megabytes of it, never leaves the card.
struct Compare {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    count: wgpu::Buffer,
}

const COMPARE_SHADER: &str = r#"
@group(0) @binding(0) var fresh: texture_2d<f32>;
@group(0) @binding(1) var kept: texture_2d<f32>;
@group(0) @binding(2) var<storage, read_write> n: atomic<u32>;
@compute @workgroup_size(16, 16) fn main(@builtin(global_invocation_id) g: vec3<u32>) {
    let d = textureDimensions(fresh);
    let p = g.xy * 3u;
    if p.x < d.x && p.y < d.y {
        let a = textureLoad(fresh, vec2<i32>(p), 0).rgb;
        let b = textureLoad(kept, vec2<i32>(p), 0).rgb;
        let m = max(abs(a.r - b.r), max(abs(a.g - b.g), abs(a.b - b.b)));
        if m > 3.5 / 255.0 {
            atomicAdd(&n, 1u);
        }
    }
}
"#;

impl Compare {
    fn new(d: &wgpu::Device) -> Compare {
        let tex = |binding| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None };
        let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("a capture compared"),
            entries: &[tex(0), tex(1), wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: false }, has_dynamic_offset: false, min_binding_size: wgpu::BufferSize::new(4) }, count: None }],
        });
        let pl = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("a capture compared"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let shader = d.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("a capture compared"), source: wgpu::ShaderSource::Wgsl(COMPARE_SHADER.into()) });
        let pipeline = d.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some("a capture compared"), layout: Some(&pl), module: &shader, entry_point: Some("main"), compilation_options: Default::default(), cache: None });
        let count = d.create_buffer(&wgpu::BufferDescriptor { label: Some("pixels that differ"), size: 4, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        Compare { pipeline, layout, count }
    }

    /// `true` if they differ, or if the card could not say.
    fn differs(&self, g: &crate::gpu::Gpu, fresh: &wgpu::Texture, kept: &wgpu::Texture, size: (u32, u32)) -> bool {
        let (dev, queue) = (&g.device, &g.queue);
        let (fv, kv) = (fresh.create_view(&Default::default()), kept.create_view(&Default::default()));
        let group = dev.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&fv) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&kv) },
                wgpu::BindGroupEntry { binding: 2, resource: self.count.as_entire_binding() },
            ],
        });
        queue.write_buffer(&self.count, 0, &[0; 4]);
        let mut enc = dev.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("a capture compared") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("a capture compared"), timestamp_writes: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(size.0.div_ceil(3).div_ceil(16), size.1.div_ceil(3).div_ceil(16), 1);
        }
        // A new one each time: one that was not mapped in time would still be waiting.
        let read = dev.create_buffer(&wgpu::BufferDescriptor { label: Some("pixels that differ, to read"), size: 4, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        enc.copy_buffer_to_buffer(&self.count, 0, &read, 0, 4);
        let index = queue.submit([enc.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        read.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r.is_ok());
        });
        let waited = dev.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(std::time::Duration::from_millis(50)) });
        let mapped = waited.is_ok() && rx.recv_timeout(std::time::Duration::from_millis(50)).unwrap_or(false);
        if !mapped {
            // Not told in time: taken as new, and the buffer given back when it is mapped.
            return true;
        }
        let Ok(view) = read.slice(..).get_mapped_range() else { return true };
        let n = u32::from_ne_bytes(view[..4].try_into().unwrap());
        drop(view);
        read.unmap();
        n > 0
    }
}
