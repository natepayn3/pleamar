//! Programs' buffers on the card with more than one plane, and video.
//!
//! A buffer on the card may be more than one piece of memory: an RGB image
//! whose layout keeps compression data beside it (AMD, Intel), or a video
//! frame as a decoder hands it over, NV12 —the light at full size, the colour
//! at half size, apart—. wgpu only imports buffers of one plane, so these
//! are imported here with Vulkan itself, and handed to wgpu as its own
//! textures. NV12 is then painted as RGB (`YuvToRgb`) where it is used.

use crate::scene::{DmabufPiece, DmabufPlane};
use ash::vk;
use std::os::fd::{AsRawFd, IntoRawFd};
use wgpu::hal::api::Vulkan;

/// NV12 as the programs name it (DRM fourcc).
pub const NV12: u32 = u32::from_le_bytes(*b"NV12");
pub const ARGB: u32 = u32::from_le_bytes(*b"AR24");
pub const XRGB: u32 = u32::from_le_bytes(*b"XR24");
/// RGBA in memory (Firefox's and Chromium's tiles).
pub const ABGR: u32 = u32::from_le_bytes(*b"AB24");
pub const XBGR: u32 = u32::from_le_bytes(*b"XB24");
/// `DRM_FORMAT_MOD_INVALID`: a buffer whose layout its driver chose without naming it.
pub const NO_MODIFIER: u64 = 0x00ff_ffff_ffff_ffff;

/// Whether the render can take buffers in that format: BGRA, RGBA, or NV12
/// if the card was opened with it.
pub fn readable(device: &wgpu::Device, fourcc: u32) -> bool {
    [ARGB, XRGB, ABGR, XBGR].contains(&fourcc) || (fourcc == NV12 && device.features().contains(wgpu::Features::TEXTURE_FORMAT_NV12))
}

/// Whether a buffer shows no transparency: XRGB, XBGR and video.
pub fn opaque(fourcc: u32) -> bool {
    fourcc == XRGB || fourcc == XBGR || fourcc == NV12
}

/// Whether it is copied as it is into BGRA, or has to be painted there
/// (RGBA, video).
pub fn copied(fourcc: u32) -> bool {
    fourcc == ARGB || fourcc == XRGB
}

/// A program's buffer, all its planes, as a texture of this device: BGRA
/// for ARGB and XRGB, RGBA for ABGR and XBGR, NV12 as it is (paint the last
/// two into BGRA with `YuvToRgb`).
pub fn import(device: &wgpu::Device, d: DmabufPiece, size: (u32, u32), uses: wgpu::TextureUses, usage: wgpu::TextureUsages, initial: wgpu::TextureUses) -> Result<wgpu::Texture, String> {
    let nv12 = d.fourcc == NV12;
    if !readable(device, d.fourcc) {
        return Err(format!("the format {:#x} is not read", d.fourcc));
    }
    // One plane of BGRA: as wgpu does it.
    if copied(d.fourcc) && d.planes.len() == 1 {
        let p = d.planes.into_iter().next().unwrap();
        return crate::gpu::Gpu::import_dmabuf(device, p.fd, size, d.modifier, p.stride, p.offset, uses, usage, initial);
    }
    let (format, vk_format) = if nv12 {
        (wgpu::TextureFormat::NV12, vk::Format::G8_B8R8_2PLANE_420_UNORM)
    } else if d.fourcc == ABGR || d.fourcc == XBGR {
        (wgpu::TextureFormat::Rgba8Unorm, vk::Format::R8G8B8A8_UNORM)
    } else {
        (wgpu::TextureFormat::Bgra8Unorm, vk::Format::B8G8R8A8_UNORM)
    };
    let extent = wgpu::Extent3d { width: size.0.max(1), height: size.1.max(1), depth_or_array_layers: 1 };
    let hal_desc = wgpu::hal::TextureDescriptor {
        label: Some("a buffer on the card, in planes"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: uses,
        memory_flags: wgpu::hal::MemoryFlags::empty(),
        view_formats: Vec::new(),
    };
    // SAFETY: the planes are dmabufs of that size, format and layout, as the
    // program says; the image and its memory are handed to wgpu, which frees
    // them (the callback) once nothing uses them.
    let hal_texture = unsafe {
        let hal = device.as_hal::<Vulkan>().ok_or("the card is not driven with Vulkan")?;
        let (image, memories) = import_planes(&hal, d.planes, vk_format, extent, d.modifier, nv12, uses)?;
        let raw = hal.raw_device().clone();
        let free: wgpu::hal::DropCallback = Box::new(move || {
            raw.destroy_image(image, None);
            for m in memories {
                raw.free_memory(m, None);
            }
        });
        hal.texture_from_raw(image, &hal_desc, Some(free), wgpu::hal::vulkan::TextureMemory::External)
    };
    let desc = wgpu::TextureDescriptor { label: Some("a buffer on the card, in planes"), size: extent, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format, usage, view_formats: &[] };
    // SAFETY: made on this device, as `desc` says.
    Ok(unsafe { device.create_texture_from_hal::<Vulkan>(hal_texture, &desc, initial) })
}

/// The image, with its memory: one piece if all the planes are in the same
/// buffer (the usual), one per plane if not (a «disjoint» image).
unsafe fn import_planes(hal: &wgpu::hal::vulkan::Device, planes: Vec<DmabufPlane>, format: vk::Format, extent: wgpu::Extent3d, modifier: u64, nv12: bool, uses: wgpu::TextureUses) -> Result<(vk::Image, Vec<vk::DeviceMemory>), String> {
    let raw = hal.raw_device();
    let instance = hal.shared_instance().raw_instance();
    let fd_fn = ash::khr::external_memory_fd::Device::new(instance, raw);
    // The same buffer if every plane's fd is the same file.
    let id = |p: &DmabufPlane| {
        // SAFETY: fstat into a zeroed struct of the right type.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        (unsafe { libc::fstat(p.fd.as_raw_fd(), &mut st) } == 0).then_some((st.st_dev, st.st_ino))
    };
    let first = id(&planes[0]);
    let disjoint = first.is_none() || planes.iter().any(|p| id(p) != first);
    let layouts: Vec<vk::SubresourceLayout> = planes.iter().map(|p| vk::SubresourceLayout { offset: p.offset as u64, row_pitch: p.stride as u64, size: 0, array_pitch: 0, depth_pitch: 0 }).collect();
    let mut drm = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default().drm_format_modifier(modifier).plane_layouts(&layouts);
    let mut external = vk::ExternalMemoryImageCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    // NV12 is read one plane at a time: light as R8, colour as RG8.
    let view_formats = [format, vk::Format::R8_UNORM, vk::Format::R8G8_UNORM];
    let mut list = vk::ImageFormatListCreateInfo::default().view_formats(&view_formats);
    let mut flags = vk::ImageCreateFlags::empty();
    if nv12 {
        flags |= vk::ImageCreateFlags::MUTABLE_FORMAT;
    }
    if disjoint {
        flags |= vk::ImageCreateFlags::DISJOINT;
    }
    let mut usage = vk::ImageUsageFlags::empty();
    if uses.contains(wgpu::TextureUses::COPY_SRC) {
        usage |= vk::ImageUsageFlags::TRANSFER_SRC;
    }
    if uses.contains(wgpu::TextureUses::RESOURCE) {
        usage |= vk::ImageUsageFlags::SAMPLED;
    }
    let mut info = vk::ImageCreateInfo::default()
        .flags(flags)
        .image_type(vk::ImageType::TYPE_2D)
        .format(format)
        .extent(vk::Extent3D { width: extent.width, height: extent.height, depth: 1 })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(usage)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut external)
        .push_next(&mut drm);
    if nv12 {
        info = info.push_next(&mut list);
    }
    // SAFETY: a valid create info for this device.
    let image = unsafe { raw.create_image(&info, None) }.map_err(|e| format!("the card did not take the buffer's layout: {e}"))?;
    let aspects = [vk::ImageAspectFlags::MEMORY_PLANE_0_EXT, vk::ImageAspectFlags::MEMORY_PLANE_1_EXT, vk::ImageAspectFlags::MEMORY_PLANE_2_EXT, vk::ImageAspectFlags::MEMORY_PLANE_3_EXT];
    let mut memories = Vec::new();
    let fail = |memories: &[vk::DeviceMemory], e: String| {
        // SAFETY: made here and not handed to anyone.
        unsafe {
            for m in memories {
                raw.free_memory(*m, None);
            }
            raw.destroy_image(image, None);
        }
        Err(e)
    };
    if disjoint {
        let mut binds = Vec::new();
        for (k, p) in planes.into_iter().enumerate().take(4) {
            let mut plane = vk::ImagePlaneMemoryRequirementsInfo::default().plane_aspect(aspects[k]);
            let req_info = vk::ImageMemoryRequirementsInfo2::default().image(image).push_next(&mut plane);
            let mut req = vk::MemoryRequirements2::default();
            // SAFETY: the image was just made, disjoint, with that plane.
            unsafe { raw.get_image_memory_requirements2(&req_info, &mut req) };
            match unsafe { import_memory(raw, &fd_fn, p.fd.into_raw_fd(), req.memory_requirements, None) } {
                Ok(m) => {
                    memories.push(m);
                    binds.push((m, aspects[k]));
                }
                Err(e) => return fail(&memories, e),
            }
        }
        let mut plane_infos: Vec<vk::BindImagePlaneMemoryInfo> = binds.iter().map(|(_, a)| vk::BindImagePlaneMemoryInfo::default().plane_aspect(*a)).collect();
        let infos: Vec<vk::BindImageMemoryInfo> = binds.iter().zip(plane_infos.iter_mut()).map(|((m, _), pi)| vk::BindImageMemoryInfo::default().image(image).memory(*m).memory_offset(0).push_next(pi)).collect();
        // SAFETY: each plane's memory, imported for it.
        if let Err(e) = unsafe { raw.bind_image_memory2(&infos) } {
            return fail(&memories, format!("the planes could not be bound: {e}"));
        }
    } else {
        // SAFETY: the image was just made on this device.
        let req = unsafe { raw.get_image_memory_requirements(image) };
        let fd = planes.into_iter().next().unwrap().fd.into_raw_fd();
        match unsafe { import_memory(raw, &fd_fn, fd, req, Some(image)) } {
            Ok(m) => memories.push(m),
            Err(e) => return fail(&memories, e),
        }
        // SAFETY: memory imported for this image.
        if let Err(e) = unsafe { raw.bind_image_memory(image, memories[0], 0) } {
            return fail(&memories, format!("the buffer could not be bound: {e}"));
        }
    }
    Ok((image, memories))
}

/// A dmabuf as memory of the card, for that image (dedicated, if it is not
/// disjoint). The fd is the card's from here on, or closed if it fails.
unsafe fn import_memory(raw: &ash::Device, fd_fn: &ash::khr::external_memory_fd::Device, fd: i32, req: vk::MemoryRequirements, dedicated: Option<vk::Image>) -> Result<vk::DeviceMemory, String> {
    let close = || {
        // SAFETY: the fd is ours until the card takes it.
        unsafe { libc::close(fd) };
    };
    let mut props = vk::MemoryFdPropertiesKHR::default();
    // SAFETY: a dmabuf fd, asked about.
    if let Err(e) = unsafe { fd_fn.get_memory_fd_properties(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT, fd, &mut props) } {
        close();
        return Err(format!("the card does not know the buffer: {e}"));
    }
    let bits = req.memory_type_bits & props.memory_type_bits;
    if bits == 0 {
        close();
        return Err("no kind of the card's memory fits the buffer".into());
    }
    let mut import = vk::ImportMemoryFdInfoKHR::default().handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT).fd(fd);
    let mut one = vk::MemoryDedicatedAllocateInfo::default();
    if let Some(image) = dedicated {
        one = one.image(image);
    }
    let mut info = vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(bits.trailing_zeros()).push_next(&mut import);
    if dedicated.is_some() {
        info = info.push_next(&mut one);
    }
    // SAFETY: a valid allocation that imports the fd.
    unsafe { raw.allocate_memory(&info, None) }.map_err(|e| {
        close();
        format!("the buffer could not be imported: {e}")
    })
}

/// Video (NV12) painted as RGB: light and colour, read apart, to the colours
/// a monitor shows. BT.709 for high definition, BT.601 below, limited range,
/// as decoders hand them over.
pub struct YuvToRgb {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    /// RGBA painted as it is: into BGRA, which a copy cannot do.
    plain: wgpu::RenderPipeline,
    plain_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

const PLAIN_SHADER: &str = r#"
struct V { @builtin(position) p: vec4<f32>, @location(0) uv: vec2<f32> };
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;
@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return V(vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0), uv);
}
@fragment fn fs(v: V) -> @location(0) vec4<f32> {
    return textureSample(t, s, v.uv);
}
"#;

const YUV_SHADER: &str = r#"
struct V { @builtin(position) p: vec4<f32>, @location(0) uv: vec2<f32> };
@group(0) @binding(0) var light: texture_2d<f32>;
@group(0) @binding(1) var colour: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;
@group(0) @binding(3) var<uniform> hd: vec4<f32>;
@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return V(vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0), uv);
}
@fragment fn fs(v: V) -> @location(0) vec4<f32> {
    let y = (textureSample(light, s, v.uv).r - 16.0 / 255.0) * (255.0 / 219.0);
    let c = (textureSample(colour, s, v.uv).rg - vec2<f32>(128.0 / 255.0)) * (255.0 / 224.0);
    var rgb: vec3<f32>;
    if hd.x > 0.5 {
        rgb = vec3<f32>(y + 1.5748 * c.y, y - 0.1873 * c.x - 0.4681 * c.y, y + 1.8556 * c.x);
    } else {
        rgb = vec3<f32>(y + 1.402 * c.y, y - 0.344136 * c.x - 0.714136 * c.y, y + 1.772 * c.x);
    }
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
"#;

impl YuvToRgb {
    /// Painting into textures of that format.
    pub fn new(device: &wgpu::Device, target: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("video to RGB"), source: wgpu::ShaderSource::Wgsl(YUV_SHADER.into()) });
        let tex = |binding| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video to RGB"),
            entries: &[
                tex(0),
                tex(1),
                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
                wgpu::BindGroupLayoutEntry { binding: 3, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("video to RGB"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video to RGB"),
            layout: Some(&pl),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), buffers: &[], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs"), targets: &[Some(wgpu::ColorTargetState { format: target, blend: None, write_mask: wgpu::ColorWrites::ALL })], compilation_options: Default::default() }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let plain_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("RGBA to BGRA"), source: wgpu::ShaderSource::Wgsl(PLAIN_SHADER.into()) });
        let plain_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("RGBA to BGRA"),
            entries: &[tex(0), wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None }],
        });
        let ppl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("RGBA to BGRA"), bind_group_layouts: &[Some(&plain_layout)], immediate_size: 0 });
        let plain = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("RGBA to BGRA"),
            layout: Some(&ppl),
            vertex: wgpu::VertexState { module: &plain_shader, entry_point: Some("vs"), buffers: &[], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState { module: &plain_shader, entry_point: Some("fs"), targets: &[Some(wgpu::ColorTargetState { format: target, blend: None, write_mask: wgpu::ColorWrites::ALL })], compilation_options: Default::default() }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        // Nearest: pixel for pixel.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor { mag_filter: wgpu::FilterMode::Nearest, min_filter: wgpu::FilterMode::Nearest, ..Default::default() });
        YuvToRgb { pipeline, layout, plain, plain_layout, sampler }
    }

    /// The video frame `source` (NV12), painted into the corner of `target`
    /// at its size.
    pub fn convert(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, source: &wgpu::Texture, target: &wgpu::TextureView, size: (u32, u32)) {
        if source.format() != wgpu::TextureFormat::NV12 {
            let view = source.create_view(&Default::default());
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.plain_layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) }],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("RGBA to BGRA"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: target, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.plain);
            pass.set_bind_group(0, &group, &[]);
            pass.set_viewport(0.0, 0.0, size.0.max(1) as f32, size.1.max(1) as f32, 0.0, 1.0);
            pass.draw(0..3, 0..1);
            return;
        }
        let plane = |format, aspect| source.create_view(&wgpu::TextureViewDescriptor { format: Some(format), aspect, dimension: Some(wgpu::TextureViewDimension::D2), ..Default::default() });
        let light = plane(wgpu::TextureFormat::R8Unorm, wgpu::TextureAspect::Plane0);
        let colour = plane(wgpu::TextureFormat::Rg8Unorm, wgpu::TextureAspect::Plane1);
        let hd: [f32; 4] = [if size.1 >= 720 { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0];
        let buffer = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage: wgpu::BufferUsages::UNIFORM, mapped_at_creation: true });
        if let Ok(mut view) = buffer.slice(..).get_mapped_range_mut() {
            view.copy_from_slice(bytemuck::cast_slice(&hd));
        }
        buffer.unmap();
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&light) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&colour) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: buffer.as_entire_binding() },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("video to RGB"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: target, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.set_viewport(0.0, 0.0, size.0.max(1) as f32, size.1.max(1) as f32, 0.0, 1.0);
        pass.draw(0..3, 0..1);
    }
}

/// A live window's frame on the card (`thumbnails.live`) made the size it is
/// drawn at, straight into the atlas: what `text::fit_frame` does in memory
/// with a frame copied there, the same way —each pixel the mean of the box of
/// the window's it covers, or filtered when made larger; colours kept
/// premultiplied; alpha 1 for a window without it—, and the rest of the slot
/// and its gutter transparent.
pub struct FitFrame {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

const FIT_SHADER: &str = r#"
struct Fit { inner: vec4<f32>, source: vec2<f32>, opaque: f32, pad: f32 };
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;
@group(0) @binding(2) var<uniform> fit: Fit;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
}
@fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let o = p.xy - fit.inner.xy;
    let n = fit.inner.zw;
    if o.x < 0.0 || o.y < 0.0 || o.x >= n.x || o.y >= n.y {
        return vec4<f32>(0.0);
    }
    var c: vec4<f32>;
    if n.x <= fit.source.x && n.y <= fit.source.y {
        // The window's pixels this one covers, as the workshop counts them;
        // at most 16 a side read, evenly spread, for a large window made very small.
        let i = floor(o);
        let a = floor(i * fit.source / n);
        let b = max(floor((i + 1.0) * fit.source / n), a + 1.0);
        let step = max(vec2<f32>(1.0), ceil((b - a) / 16.0));
        var sum = vec4<f32>(0.0);
        var count = 0.0;
        for (var y = a.y; y < b.y; y += step.y) {
            for (var x = a.x; x < b.x; x += step.x) {
                sum += textureLoad(t, vec2<i32>(i32(x), i32(y)), 0);
                count += 1.0;
            }
        }
        c = sum / count;
    } else {
        c = textureSampleLevel(t, s, o / n, 0.0);
    }
    if fit.opaque > 0.5 {
        c.a = 1.0;
    }
    return c;
}
"#;

impl FitFrame {
    /// Painting into textures of that format (the atlas's).
    pub fn new(device: &wgpu::Device, target: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("a live window made small"), source: wgpu::ShaderSource::Wgsl(FIT_SHADER.into()) });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("a live window made small"),
            entries: &[
                wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("a live window made small"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("a live window made small"),
            layout: Some(&pl),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), buffers: &[], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs"), targets: &[Some(wgpu::ColorTargetState { format: target, blend: None, write_mask: wgpu::ColorWrites::ALL })], compilation_options: Default::default() }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        // Linear, for a window made larger (the workshop's triangle filter).
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor { mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
        FitFrame { pipeline, layout, sampler }
    }

    /// `source` (the window, BGRA) into `inner` of `target`, and the rest of
    /// `padded` transparent: both `[x, y, width, height]` in its pixels.
    pub fn fit(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, source: &wgpu::Texture, target: &wgpu::TextureView, (padded, inner): ([u32; 4], [u32; 4]), opaque: bool) {
        let view = source.create_view(&Default::default());
        let values: [f32; 8] = [inner[0] as f32, inner[1] as f32, inner[2] as f32, inner[3] as f32, source.width() as f32, source.height() as f32, if opaque { 1.0 } else { 0.0 }, 0.0];
        let buffer = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 32, usage: wgpu::BufferUsages::UNIFORM, mapped_at_creation: true });
        if let Ok(mut m) = buffer.slice(..).get_mapped_range_mut() {
            m.copy_from_slice(bytemuck::cast_slice(&values));
        }
        buffer.unmap();
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: buffer.as_entire_binding() },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("a live window made small"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: target, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &group, &[]);
        let [x, y, w, h] = padded;
        pass.set_viewport(x as f32, y as f32, w.max(1) as f32, h.max(1) as f32, 0.0, 1.0);
        pass.set_scissor_rect(x, y, w.max(1), h.max(1));
        pass.draw(0..3, 0..1);
    }
}

/// Eight bytes that tell a live window's frame on the card from the last one,
/// without reading it back: Hyprland fills a waiting frame each time the
/// monitor redraws (the overview drawing it, too), and damages all of it every
/// time, so only the pixels can say it is the same.
pub struct HashFrame {
    /// For a frame whose alpha counts, and for one whose alpha is anything (XRGB).
    pipelines: [wgpu::ComputePipeline; 2],
    layout: wgpu::BindGroupLayout,
}

const HASH_SHADER: &str = r#"
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> out: array<atomic<u32>, 2>;
var<workgroup> a: atomic<u32>;
var<workgroup> b: atomic<u32>;
fn mix(x: u32) -> u32 {
    var h = x;
    h ^= h >> 16u;
    h *= 0x7feb352du;
    h ^= h >> 15u;
    h *= 0x846ca68bu;
    h ^= h >> 16u;
    return h;
}
@compute @workgroup_size(16, 16) fn main(@builtin(global_invocation_id) g: vec3<u32>, @builtin(local_invocation_index) l: u32) {
    let n = textureDimensions(t);
    if g.x < n.x && g.y < n.y {
        // XRGB's alpha byte is anything: colour only there. ARGB's counts, as
        // see-through black and opaque black have the same colour.
        let p = pack4x8unorm(textureLoad(t, vec2<i32>(g.xy), 0)) & MASK;
        let i = g.y * n.x + g.x;
        // Each pixel mixed with where it is, so two pixels swapped are a change.
        atomicAdd(&a, mix(p ^ (i * 0x9e3779b9u)));
        atomicAdd(&b, mix((p + i * 0x85ebca6bu) ^ 0x27d4eb2fu));
    }
    workgroupBarrier();
    if l == 0u {
        atomicAdd(&out[0], atomicLoad(&a));
        atomicAdd(&out[1], atomicLoad(&b));
    }
}
"#;

impl HashFrame {
    pub fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("a live window's frame told apart"),
            entries: &[
                wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: false }, has_dynamic_offset: false, min_binding_size: wgpu::BufferSize::new(8) }, count: None },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("a live window's frame told apart"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let pipelines = ["0xffffffffu", "0x00ffffffu"].map(|mask| {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("a live window's frame told apart"), source: wgpu::ShaderSource::Wgsl(HASH_SHADER.replace("MASK", mask).into()) });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some("a live window's frame told apart"), layout: Some(&pl), module: &shader, entry_point: Some("main"), compilation_options: Default::default(), cache: None })
        });
        HashFrame { pipelines, layout }
    }

    /// `source`'s pixels into the 8 bytes of `out` at `offset`, which must be zero
    /// before (and aligned for storage); its alpha left out if `opaque`.
    pub fn hash(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, source: &wgpu::Texture, opaque: bool, (out, offset): (&wgpu::Buffer, u64)) {
        let view = source.create_view(&Default::default());
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: out, offset, size: wgpu::BufferSize::new(8) }) },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("a live window's frame told apart"), timestamp_writes: None });
        pass.set_pipeline(&self.pipelines[opaque as usize]);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(source.width().div_ceil(16), source.height().div_ceil(16), 1);
    }
}

/// The modifiers the card can read a format with, for that use (any number of planes).
pub fn modifiers(adapter: &wgpu::Adapter, format: vk::Format, need: vk::FormatFeatureFlags) -> Vec<u64> {
    // SAFETY: only reading properties of the physical device wgpu uses.
    unsafe {
        let Some(hal) = adapter.as_hal::<Vulkan>() else { return Vec::new() };
        let instance = hal.shared_instance().raw_instance();
        let pd = hal.raw_physical_device();
        let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
        let mut fp = vk::FormatProperties2::default().push_next(&mut list);
        instance.get_physical_device_format_properties2(pd, format, &mut fp);
        let n = list.drm_format_modifier_count as usize;
        let mut mods = vec![vk::DrmFormatModifierPropertiesEXT::default(); n];
        let mut list = vk::DrmFormatModifierPropertiesListEXT::default().drm_format_modifier_properties(&mut mods);
        let mut fp = vk::FormatProperties2::default().push_next(&mut list);
        instance.get_physical_device_format_properties2(pd, format, &mut fp);
        mods.iter().filter(|m| m.drm_format_modifier_tiling_features.contains(need)).map(|m| m.drm_format_modifier).collect()
    }
}

/// A frame of our own on the card, to hand to the compositor as a dmabuf:
/// the texture painted in, and how the compositor reads its memory.
pub struct Exported {
    pub texture: wgpu::Texture,
    pub fd: std::os::fd::OwnedFd,
    pub modifier: u64,
    /// Each plane's offset and stride, in bytes: one, for the layouts asked for.
    pub planes: Vec<(u32, u32)>,
}

/// A BGRA texture of that size the card paints in and the compositor reads
/// (ARGB8888 to it), in one of `modifiers`. Only layouts of one plane are
/// asked for: those with the compression data beside it (AMD's DCC, Intel's
/// CCS) need the image handed over to the other process with a barrier wgpu
/// does not make, and a frame cleared to transparent could come out garbled.
pub fn export(device: &wgpu::Device, size: (u32, u32), modifiers: &[u64]) -> Result<Exported, String> {
    export_as(device, size, modifiers, false, false)
}

/// The same, the other way round: a texture the compositor paints in (a
/// window it copies for `thumbnails.live`) and the card reads from. Its
/// content is the compositor's, never cleared here.
pub fn export_to_read(device: &wgpu::Device, size: (u32, u32), modifiers: &[u64]) -> Result<Exported, String> {
    export_as(device, size, modifiers, true, false)
}

/// The same, and the card can also copy from it: where the compositor leaves
/// a capture of what is behind a glass (`lens.rs`), which is copied on the
/// card to the lens's own texture, never read back by the processor.
pub fn export_to_capture(device: &wgpu::Device, size: (u32, u32), modifiers: &[u64]) -> Result<Exported, String> {
    export_as(device, size, modifiers, true, true)
}

fn export_as(device: &wgpu::Device, size: (u32, u32), modifiers: &[u64], read: bool, copy_src: bool) -> Result<Exported, String> {
    let extent = wgpu::Extent3d { width: size.0.max(1), height: size.1.max(1), depth_or_array_layers: 1 };
    let uses = if read { wgpu::TextureUses::RESOURCE | if copy_src { wgpu::TextureUses::COPY_SRC } else { wgpu::TextureUses::empty() } } else { wgpu::TextureUses::COLOR_TARGET | wgpu::TextureUses::COPY_DST };
    let hal_desc = wgpu::hal::TextureDescriptor {
        label: Some("a frame of ours"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: uses,
        memory_flags: wgpu::hal::MemoryFlags::empty(),
        view_formats: Vec::new(),
    };
    // SAFETY: an image made here, with memory made for it; both handed to
    // wgpu, which frees them (the callback) once nothing uses them.
    let (hal_texture, fd, modifier, planes) = unsafe {
        let hal = device.as_hal::<Vulkan>().ok_or("the card is not driven with Vulkan")?;
        let (image, memory, fd, modifier, planes) = export_image(&hal, extent, modifiers, read, copy_src)?;
        let raw = hal.raw_device().clone();
        let free: wgpu::hal::DropCallback = Box::new(move || {
            raw.destroy_image(image, None);
            raw.free_memory(memory, None);
        });
        (hal.texture_from_raw(image, &hal_desc, Some(free), wgpu::hal::vulkan::TextureMemory::External), fd, modifier, planes)
    };
    let desc = wgpu::TextureDescriptor {
        label: Some("a frame of ours"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: if read { wgpu::TextureUsages::TEXTURE_BINDING | if copy_src { wgpu::TextureUsages::COPY_SRC } else { wgpu::TextureUsages::empty() } } else { wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST },
        view_formats: &[],
    };
    // SAFETY: made on this device, as `desc` says.
    let texture = unsafe { device.create_texture_from_hal::<Vulkan>(hal_texture, &desc, if read { uses } else { wgpu::TextureUses::UNINITIALIZED }) };
    Ok(Exported { texture, fd, modifier, planes })
}

/// The image, its memory, the memory as a dmabuf, the layout the card chose
/// and its planes.
type ExportedImage = (vk::Image, vk::DeviceMemory, std::os::fd::OwnedFd, u64, Vec<(u32, u32)>);

unsafe fn export_image(hal: &wgpu::hal::vulkan::Device, extent: wgpu::Extent3d, modifiers: &[u64], read: bool, copy_src: bool) -> Result<ExportedImage, String> {
    use std::os::fd::FromRawFd;
    let raw = hal.raw_device();
    let instance = hal.shared_instance().raw_instance();
    let pd = hal.raw_physical_device();
    // Of the layouts the compositor takes, those the card paints in (or reads
    // from), of one plane.
    let (usage, need) = if read && copy_src {
        (vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC, vk::FormatFeatureFlags::SAMPLED_IMAGE | vk::FormatFeatureFlags::TRANSFER_SRC)
    } else if read {
        (vk::ImageUsageFlags::SAMPLED, vk::FormatFeatureFlags::SAMPLED_IMAGE)
    } else {
        (vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_DST, vk::FormatFeatureFlags::COLOR_ATTACHMENT | vk::FormatFeatureFlags::TRANSFER_DST)
    };
    // SAFETY: only reading the physical device's properties.
    let one_plane: Vec<u64> = unsafe { plane_counts(instance, pd, vk::Format::B8G8R8A8_UNORM) }
        .into_iter()
        .filter(|(m, planes, features)| *planes == 1 && features.contains(need) && modifiers.contains(m))
        .map(|(m, _, _)| m)
        .collect();
    if one_plane.is_empty() {
        return Err(format!("the compositor takes no layout of one plane the card {}", if read { "reads" } else { "paints in" }));
    }
    let mut list = vk::ImageDrmFormatModifierListCreateInfoEXT::default().drm_format_modifiers(&one_plane);
    let mut external = vk::ExternalMemoryImageCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::B8G8R8A8_UNORM)
        .extent(vk::Extent3D { width: extent.width, height: extent.height, depth: 1 })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(usage)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut external)
        .push_next(&mut list);
    // SAFETY: a valid create info for this device.
    let image = unsafe { raw.create_image(&info, None) }.map_err(|e| format!("the card made no frame in those layouts: {e}"))?;
    let fail = |memory: Option<vk::DeviceMemory>, e: String| {
        // SAFETY: made here and not handed to anyone.
        unsafe {
            if let Some(m) = memory {
                raw.free_memory(m, None);
            }
            raw.destroy_image(image, None);
        }
        Err(e)
    };
    let modifier_fn = ash::ext::image_drm_format_modifier::Device::new(instance, raw);
    let mut chosen = vk::ImageDrmFormatModifierPropertiesEXT::default();
    // SAFETY: an image made with a list of modifiers.
    if let Err(e) = unsafe { modifier_fn.get_image_drm_format_modifier_properties(image, &mut chosen) } {
        return fail(None, format!("the card did not say which layout it chose: {e}"));
    }
    // SAFETY: the image was just made on this device.
    let req = unsafe { raw.get_image_memory_requirements(image) };
    // SAFETY: only reading the physical device's properties.
    let kinds = unsafe { instance.get_physical_device_memory_properties(pd) };
    let fits = |k: u32| req.memory_type_bits & (1 << k) != 0;
    let local = (0..kinds.memory_type_count).find(|k| fits(*k) && kinds.memory_types[*k as usize].property_flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL));
    let Some(kind) = local.or_else(|| (0..kinds.memory_type_count).find(|k| fits(*k))) else {
        return fail(None, "no kind of the card's memory fits the frame".into());
    };
    let mut export = vk::ExportMemoryAllocateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let mut one = vk::MemoryDedicatedAllocateInfo::default().image(image);
    let alloc = vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(kind).push_next(&mut export).push_next(&mut one);
    // SAFETY: a valid allocation, dedicated to the image.
    let memory = match unsafe { raw.allocate_memory(&alloc, None) } {
        Ok(m) => m,
        Err(e) => return fail(None, format!("no memory for the frame: {e}")),
    };
    // SAFETY: memory made for this image.
    if let Err(e) = unsafe { raw.bind_image_memory(image, memory, 0) } {
        return fail(Some(memory), format!("the frame's memory could not be bound: {e}"));
    }
    let fd_fn = ash::khr::external_memory_fd::Device::new(instance, raw);
    // SAFETY: memory allocated to be exported as a dmabuf.
    let fd = match unsafe { fd_fn.get_memory_fd(&vk::MemoryGetFdInfoKHR::default().memory(memory).handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)) } {
        // SAFETY: a new fd, ours.
        Ok(fd) => unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) },
        Err(e) => return fail(Some(memory), format!("the frame could not be shared: {e}")),
    };
    // SAFETY: an image with a modifier of one plane.
    let layout = unsafe { raw.get_image_subresource_layout(image, vk::ImageSubresource { aspect_mask: vk::ImageAspectFlags::MEMORY_PLANE_0_EXT, mip_level: 0, array_layer: 0 }) };
    Ok((image, memory, fd, chosen.drm_format_modifier, vec![(layout.offset as u32, layout.row_pitch as u32)]))
}

/// Each layout the card knows for that format: its modifier, how many
/// planes it has and what can be done with it.
unsafe fn plane_counts(instance: &ash::Instance, pd: vk::PhysicalDevice, format: vk::Format) -> Vec<(u64, u32, vk::FormatFeatureFlags)> {
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
    let mut fp = vk::FormatProperties2::default().push_next(&mut list);
    // SAFETY: only reading properties.
    unsafe { instance.get_physical_device_format_properties2(pd, format, &mut fp) };
    let n = list.drm_format_modifier_count as usize;
    let mut mods = vec![vk::DrmFormatModifierPropertiesEXT::default(); n];
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default().drm_format_modifier_properties(&mut mods);
    let mut fp = vk::FormatProperties2::default().push_next(&mut list);
    // SAFETY: only reading properties.
    unsafe { instance.get_physical_device_format_properties2(pd, format, &mut fp) };
    mods.iter().map(|m| (m.drm_format_modifier, m.drm_format_modifier_plane_count, m.drm_format_modifier_tiling_features)).collect()
}

/// A buffer on the card (a monitor's, made with GBM) as a texture of a device
/// that runs on OpenGL: EGL takes the buffer (`EGL_EXT_image_dma_buf_import`)
/// and a GL texture is bound to it, which wgpu then uses as its own. It is how
/// a card whose Vulkan driver names no layouts —AMD's GFX6 to GFX8, an RX 580,
/// until Linux 7.3 and Mesa 26.3— still paints straight into its monitors.
/// `NO_MODIFIER` (`DRM_FORMAT_MOD_INVALID`): the layout its driver chose
/// without saying, which EGL is then not told either.
///
/// First written by prehonor, for pleamar-wm issue 2.
pub fn import_gl(device: &wgpu::Device, fd: std::os::fd::OwnedFd, size: (u32, u32), fourcc: u32, modifier: u64, stride: u32, offset: u32) -> Result<wgpu::Texture, String> {
    use glow::HasContext;
    use std::os::fd::AsRawFd;
    const EGL_LINUX_DMA_BUF_EXT: u32 = 0x3270;
    const EGL_LINUX_DRM_FOURCC_EXT: i32 = 0x3271;
    const EGL_DMA_BUF_PLANE0_FD_EXT: i32 = 0x3272;
    const EGL_DMA_BUF_PLANE0_OFFSET_EXT: i32 = 0x3273;
    const EGL_DMA_BUF_PLANE0_PITCH_EXT: i32 = 0x3274;
    const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: i32 = 0x3443;
    const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: i32 = 0x3444;
    const EGL_WIDTH: i32 = 0x3057;
    const EGL_HEIGHT: i32 = 0x3056;
    const EGL_NONE: i32 = 0x3038;
    type CreateImage = unsafe extern "system" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, u32, *mut std::ffi::c_void, *const i32) -> *mut std::ffi::c_void;
    type DestroyImage = unsafe extern "system" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> u32;
    type ImageTargetTexture = unsafe extern "system" fn(u32, *mut std::ffi::c_void);

    // SAFETY: the device is asked for its OpenGL side only to use its own
    // context, locked (made current) while GL is spoken to.
    let hal = unsafe { device.as_hal::<wgpu::hal::api::Gles>() }.ok_or("the device does not run on OpenGL")?;
    let context = hal.context();
    let egl = context.egl_instance().ok_or("the device has no EGL of its own")?;
    let display = context.raw_display().ok_or("the device has no EGL display")?.as_ptr();
    let lookup = |name: &str| egl.get_proc_address(name).map(|f| f as *const ());
    let create = lookup("eglCreateImageKHR").ok_or("EGL cannot make images (eglCreateImageKHR)")?;
    let destroy = lookup("eglDestroyImageKHR").ok_or("EGL cannot destroy images (eglDestroyImageKHR)")?;
    let target = lookup("glEGLImageTargetTexture2DOES").ok_or("GL cannot take an EGL image (glEGLImageTargetTexture2DOES)")?;
    // SAFETY: the three are what their names say, with these signatures.
    let (create, destroy, target) = unsafe { (std::mem::transmute::<*const (), CreateImage>(create), std::mem::transmute::<*const (), DestroyImage>(destroy), std::mem::transmute::<*const (), ImageTargetTexture>(target)) };
    let mut attribs = vec![
        EGL_WIDTH,
        size.0 as i32,
        EGL_HEIGHT,
        size.1 as i32,
        EGL_LINUX_DRM_FOURCC_EXT,
        fourcc as i32,
        EGL_DMA_BUF_PLANE0_FD_EXT,
        fd.as_raw_fd(),
        EGL_DMA_BUF_PLANE0_OFFSET_EXT,
        offset as i32,
        EGL_DMA_BUF_PLANE0_PITCH_EXT,
        stride as i32,
    ];
    if modifier != NO_MODIFIER {
        attribs.extend_from_slice(&[EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT, (modifier & 0xffff_ffff) as i32, EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT, (modifier >> 32) as i32]);
    }
    attribs.push(EGL_NONE);
    let gl = context.lock();
    // SAFETY: a display of this context, no context (as the extension asks),
    // and the attributes above, ended with EGL_NONE. EGL keeps its own hold on
    // the buffer: the descriptor can be closed after.
    let image = unsafe { create(display, std::ptr::null_mut(), EGL_LINUX_DMA_BUF_EXT, std::ptr::null_mut(), attribs.as_ptr()) };
    if image.is_null() {
        return Err(format!("EGL did not take the buffer ({:?})", egl.get_error()));
    }
    // SAFETY: the context is current (locked above); the texture is bound to
    // the image, which can go once it is (the texture keeps the buffer).
    let raw = unsafe {
        let tex = gl.create_texture().map_err(|e| format!("no GL texture: {e}"))?;
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);
        target(glow::TEXTURE_2D, image);
        gl.bind_texture(glow::TEXTURE_2D, None);
        destroy(display, image);
        tex
    };
    drop(gl);
    let hal_texture = wgpu::hal::gles::Texture {
        inner: wgpu::hal::gles::TextureInner::Texture { raw, target: glow::TEXTURE_2D },
        mip_level_count: 1,
        array_layer_count: 1,
        format: wgpu::TextureFormat::Bgra8Unorm,
        format_desc: wgpu::hal::gles::TextureFormatDesc { internal: glow::RGBA8, external: glow::BGRA, data_type: glow::UNSIGNED_BYTE },
        copy_size: wgpu::hal::CopyExtent { width: size.0, height: size.1, depth: 1 },
        drop_guard: None,
    };
    let desc = wgpu::TextureDescriptor {
        label: Some("a monitor's buffer, through EGL"),
        size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    };
    // SAFETY: a texture of this device's own context, of that size and format.
    Ok(unsafe { device.create_texture_from_hal::<wgpu::hal::api::Gles>(hal_texture, &desc, wgpu::TextureUses::UNINITIALIZED) })
}
