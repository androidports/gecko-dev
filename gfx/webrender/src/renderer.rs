/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The webrender API.
//!
//! The `webrender::renderer` module provides the interface to webrender, which
//! is accessible through [`Renderer`][renderer]
//!
//! [renderer]: struct.Renderer.html

use debug_colors;
use debug_render::DebugRenderer;
use device::{DepthFunction, Device, FrameId, Program, TextureId, VertexDescriptor, GpuMarker, GpuProfiler, PBOId};
use device::{GpuSample, TextureFilter, VAOId, VertexUsageHint, FileWatcherHandler, TextureTarget, ShaderError};
use device::{get_gl_format_bgra, VertexAttribute, VertexAttributeKind};
use euclid::{Transform3D, rect};
use frame_builder::FrameBuilderConfig;
use gleam::gl;
use gpu_cache::{GpuBlockData, GpuCacheUpdate, GpuCacheUpdateList};
use internal_types::{FastHashMap, CacheTextureId, RendererFrame, ResultMsg, TextureUpdateOp};
use internal_types::{TextureUpdateList, RenderTargetMode};
use internal_types::{ORTHO_NEAR_PLANE, ORTHO_FAR_PLANE, SourceTexture};
use internal_types::{BatchTextures, TextureSampler};
use profiler::{Profiler, BackendProfileCounters};
use profiler::{GpuProfileTag, RendererProfileTimers, RendererProfileCounters};
use record::ApiRecordingReceiver;
use render_backend::RenderBackend;
use render_task::RenderTaskData;
use std;
use std::cmp;
use std::collections::VecDeque;
use std::f32;
use std::marker::PhantomData;
use std::mem;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use texture_cache::TextureCache;
use rayon::ThreadPool;
use rayon::Configuration as ThreadPoolConfig;
use tiling::{AlphaBatchKind, BlurCommand, CompositePrimitiveInstance, Frame, PrimitiveBatch, RenderTarget};
use tiling::{AlphaRenderTarget, CacheClipInstance, PrimitiveInstance, ColorRenderTarget, RenderTargetKind};
use time::precise_time_ns;
use thread_profiler::{register_thread_with_profiler, write_profile};
use util::TransformedRectKind;
use webgl_types::GLContextHandleWrapper;
use api::{ColorF, Epoch, PipelineId, RenderApiSender, RenderNotifier, RenderDispatcher};
use api::{ExternalImageId, ExternalImageType, ImageData, ImageFormat};
use api::{DeviceIntRect, DeviceUintRect, DeviceIntPoint, DeviceIntSize, DeviceUintSize};
use api::{BlobImageRenderer, channel, FontRenderMode};
use api::VRCompositorHandler;
use api::{YuvColorSpace, YuvFormat};
use api::{YUV_COLOR_SPACES, YUV_FORMATS};

pub const GPU_DATA_TEXTURE_POOL: usize = 5;
pub const MAX_VERTEX_TEXTURE_WIDTH: usize = 1024;

const GPU_TAG_CACHE_BOX_SHADOW: GpuProfileTag = GpuProfileTag { label: "C_BoxShadow", color: debug_colors::BLACK };
const GPU_TAG_CACHE_CLIP: GpuProfileTag = GpuProfileTag { label: "C_Clip", color: debug_colors::PURPLE };
const GPU_TAG_CACHE_TEXT_RUN: GpuProfileTag = GpuProfileTag { label: "C_TextRun", color: debug_colors::MISTYROSE };
const GPU_TAG_CACHE_LINE: GpuProfileTag = GpuProfileTag { label: "C_Line", color: debug_colors::BROWN };
const GPU_TAG_SETUP_TARGET: GpuProfileTag = GpuProfileTag { label: "target", color: debug_colors::SLATEGREY };
const GPU_TAG_SETUP_DATA: GpuProfileTag = GpuProfileTag { label: "data init", color: debug_colors::LIGHTGREY };
const GPU_TAG_PRIM_RECT: GpuProfileTag = GpuProfileTag { label: "Rect", color: debug_colors::RED };
const GPU_TAG_PRIM_LINE: GpuProfileTag = GpuProfileTag { label: "Line", color: debug_colors::DARKRED };
const GPU_TAG_PRIM_IMAGE: GpuProfileTag = GpuProfileTag { label: "Image", color: debug_colors::GREEN };
const GPU_TAG_PRIM_YUV_IMAGE: GpuProfileTag = GpuProfileTag { label: "YuvImage", color: debug_colors::DARKGREEN };
const GPU_TAG_PRIM_BLEND: GpuProfileTag = GpuProfileTag { label: "Blend", color: debug_colors::LIGHTBLUE };
const GPU_TAG_PRIM_HW_COMPOSITE: GpuProfileTag = GpuProfileTag { label: "HwComposite", color: debug_colors::DODGERBLUE };
const GPU_TAG_PRIM_SPLIT_COMPOSITE: GpuProfileTag = GpuProfileTag { label: "SplitComposite", color: debug_colors::DARKBLUE };
const GPU_TAG_PRIM_COMPOSITE: GpuProfileTag = GpuProfileTag { label: "Composite", color: debug_colors::MAGENTA };
const GPU_TAG_PRIM_TEXT_RUN: GpuProfileTag = GpuProfileTag { label: "TextRun", color: debug_colors::BLUE };
const GPU_TAG_PRIM_GRADIENT: GpuProfileTag = GpuProfileTag { label: "Gradient", color: debug_colors::YELLOW };
const GPU_TAG_PRIM_ANGLE_GRADIENT: GpuProfileTag = GpuProfileTag { label: "AngleGradient", color: debug_colors::POWDERBLUE };
const GPU_TAG_PRIM_RADIAL_GRADIENT: GpuProfileTag = GpuProfileTag { label: "RadialGradient", color: debug_colors::LIGHTPINK };
const GPU_TAG_PRIM_BOX_SHADOW: GpuProfileTag = GpuProfileTag { label: "BoxShadow", color: debug_colors::CYAN };
const GPU_TAG_PRIM_BORDER_CORNER: GpuProfileTag = GpuProfileTag { label: "BorderCorner", color: debug_colors::DARKSLATEGREY };
const GPU_TAG_PRIM_BORDER_EDGE: GpuProfileTag = GpuProfileTag { label: "BorderEdge", color: debug_colors::LAVENDER };
const GPU_TAG_PRIM_CACHE_IMAGE: GpuProfileTag = GpuProfileTag { label: "CacheImage", color: debug_colors::SILVER };
const GPU_TAG_BLUR: GpuProfileTag = GpuProfileTag { label: "Blur", color: debug_colors::VIOLET };

bitflags! {
    #[derive(Default)]
    pub struct DebugFlags: u32 {
        const PROFILER_DBG      = 1 << 0;
        const RENDER_TARGET_DBG = 1 << 1;
        const TEXTURE_CACHE_DBG = 1 << 2;
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PackedVertex {
    pub pos: [f32; 2],
}

const DESC_PRIM_INSTANCES: VertexDescriptor = VertexDescriptor {
    vertex_attributes: &[
        VertexAttribute { name: "aPosition", count: 2, kind: VertexAttributeKind::F32 },
    ],
    instance_attributes: &[
        VertexAttribute { name: "aData0", count: 4, kind: VertexAttributeKind::I32 },
        VertexAttribute { name: "aData1", count: 4, kind: VertexAttributeKind::I32 },
    ]
};

const DESC_BLUR: VertexDescriptor = VertexDescriptor {
    vertex_attributes: &[
        VertexAttribute { name: "aPosition", count: 2, kind: VertexAttributeKind::F32 },
    ],
    instance_attributes: &[
        VertexAttribute { name: "aBlurRenderTaskIndex", count: 1, kind: VertexAttributeKind::I32 },
        VertexAttribute { name: "aBlurSourceTaskIndex", count: 1, kind: VertexAttributeKind::I32 },
        VertexAttribute { name: "aBlurDirection", count: 1, kind: VertexAttributeKind::I32 },
    ]
};

const DESC_CLIP: VertexDescriptor = VertexDescriptor {
    vertex_attributes: &[
        VertexAttribute { name: "aPosition", count: 2, kind: VertexAttributeKind::F32 },
    ],
    instance_attributes: &[
        VertexAttribute { name: "aClipRenderTaskIndex", count: 1, kind: VertexAttributeKind::I32 },
        VertexAttribute { name: "aClipLayerIndex", count: 1, kind: VertexAttributeKind::I32 },
        VertexAttribute { name: "aClipDataIndex", count: 1, kind: VertexAttributeKind::I32 },
        VertexAttribute { name: "aClipSegmentIndex", count: 1, kind: VertexAttributeKind::I32 },
        VertexAttribute { name: "aClipResourceAddress", count: 1, kind: VertexAttributeKind::I32 },
    ]
};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VertexFormat {
    PrimitiveInstances,
    Blur,
    Clip,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GraphicsApi {
    OpenGL,
}

#[derive(Clone, Debug)]
pub struct GraphicsApiInfo {
    pub kind: GraphicsApi,
    pub renderer: String,
    pub version: String,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum ImageBufferKind {
    Texture2D = 0,
    TextureRect = 1,
    TextureExternal = 2,
}

pub const IMAGE_BUFFER_KINDS: [ImageBufferKind; 3] = [
    ImageBufferKind::Texture2D,
    ImageBufferKind::TextureRect,
    ImageBufferKind::TextureExternal
];

impl ImageBufferKind {
    pub fn get_feature_string(&self) -> &'static str {
        match *self {
            ImageBufferKind::Texture2D => "",
            ImageBufferKind::TextureRect => "TEXTURE_RECT",
            ImageBufferKind::TextureExternal => "TEXTURE_EXTERNAL",
        }
    }

    pub fn has_platform_support(&self, gl_type: &gl::GlType) -> bool {
        match *gl_type {
            gl::GlType::Gles => {
                match *self {
                    ImageBufferKind::Texture2D => true,
                    ImageBufferKind::TextureRect => true,
                    ImageBufferKind::TextureExternal => true,
                }
            }
            gl::GlType::Gl => {
                match *self {
                    ImageBufferKind::Texture2D => true,
                    ImageBufferKind::TextureRect => true,
                    ImageBufferKind::TextureExternal => false,
                }
            }
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub enum RendererKind {
    Native,
    OSMesa,
}

#[derive(Debug)]
pub struct GpuProfile {
    pub frame_id: FrameId,
    pub paint_time_ns: u64,
}

impl GpuProfile {
    fn new<T>(frame_id: FrameId, samples: &[GpuSample<T>]) -> GpuProfile {
        let mut paint_time_ns = 0;
        for sample in samples {
            paint_time_ns += sample.time_ns;
        }
        GpuProfile {
            frame_id,
            paint_time_ns,
        }
    }
}

#[derive(Debug)]
pub struct CpuProfile {
    pub frame_id: FrameId,
    pub backend_time_ns: u64,
    pub composite_time_ns: u64,
    pub draw_calls: usize,
}

impl CpuProfile {
    fn new(frame_id: FrameId,
           backend_time_ns: u64,
           composite_time_ns: u64,
           draw_calls: usize) -> CpuProfile {
        CpuProfile {
            frame_id,
            backend_time_ns,
            composite_time_ns,
            draw_calls,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum BlendMode {
    None,
    Alpha,
    PremultipliedAlpha,

    // Use the color of the text itself as a constant color blend factor.
    Subpixel(ColorF),
}

// Tracks the state of each row in the GPU cache texture.
struct CacheRow {
    is_dirty: bool,
}

impl CacheRow {
    fn new() -> CacheRow {
        CacheRow {
            is_dirty: false,
        }
    }
}

/// The device-specific representation of the cache texture in gpu_cache.rs
struct CacheTexture {
    texture_id: TextureId,
    pbo_id: PBOId,
    rows: Vec<CacheRow>,
    cpu_blocks: Vec<GpuBlockData>,
}

impl CacheTexture {
    fn new(device: &mut Device) -> CacheTexture {
        let texture_id = device.create_texture_ids(1, TextureTarget::Default)[0];
        let pbo_id = device.create_pbo();

        CacheTexture {
            texture_id,
            pbo_id,
            rows: Vec::new(),
            cpu_blocks: Vec::new(),
        }
    }

    fn apply_patch(&mut self,
                   update: &GpuCacheUpdate,
                   blocks: &[GpuBlockData]) {
        match update {
            &GpuCacheUpdate::Copy { block_index, block_count, address } => {
                let row = address.v as usize;

                // Ensure that the CPU-side shadow copy of the GPU cache data has enough
                // rows to apply this patch.
                while self.rows.len() <= row {
                    // Add a new row.
                    self.rows.push(CacheRow::new());
                    // Add enough GPU blocks for this row.
                    self.cpu_blocks.extend_from_slice(&[GpuBlockData::empty(); MAX_VERTEX_TEXTURE_WIDTH]);
                }

                // This row is dirty (needs to be updated in GPU texture).
                self.rows[row].is_dirty = true;

                // Copy the blocks from the patch array in the shadow CPU copy.
                let block_offset = row * MAX_VERTEX_TEXTURE_WIDTH + address.u as usize;
                let data = &mut self.cpu_blocks[block_offset..(block_offset + block_count)];
                for i in 0..block_count {
                    data[i] = blocks[block_index + i];
                }
            }
        }
    }

    fn update(&mut self, device: &mut Device, updates: &GpuCacheUpdateList) {
        // See if we need to create or resize the texture.
        let current_dimensions = device.get_texture_dimensions(self.texture_id);
        if updates.height > current_dimensions.height {
            // Create a f32 texture that can be used for the vertex shader
            // to fetch data from.
            device.init_texture(self.texture_id,
                                MAX_VERTEX_TEXTURE_WIDTH as u32,
                                updates.height as u32,
                                ImageFormat::RGBAF32,
                                TextureFilter::Nearest,
                                RenderTargetMode::None,
                                None);

            // Copy the current texture into the newly resized texture.
            if current_dimensions.height > 0 {
                // If we had to resize the texture, just mark all rows
                // as dirty so they will be uploaded to the texture
                // during the next flush.
                for row in &mut self.rows {
                    row.is_dirty = true;
                }
            }
        }

        for update in &updates.updates {
            self.apply_patch(update, &updates.blocks);
        }
    }

    fn flush(&mut self, device: &mut Device) {
        // Bind a PBO to do the texture upload.
        // Updating the texture via PBO avoids CPU-side driver stalls.
        device.bind_pbo(Some(self.pbo_id));

        for (row_index, row) in self.rows.iter_mut().enumerate() {
            if row.is_dirty {
                // Get the data for this row and push to the PBO.
                let block_index = row_index * MAX_VERTEX_TEXTURE_WIDTH;
                let cpu_blocks = &self.cpu_blocks[block_index..(block_index + MAX_VERTEX_TEXTURE_WIDTH)];
                device.update_pbo_data(cpu_blocks);

                // Insert a command to copy the PBO data to the right place in
                // the GPU-side cache texture.
                device.update_texture_from_pbo(self.texture_id,
                                               0,
                                               row_index as u32,
                                               MAX_VERTEX_TEXTURE_WIDTH as u32,
                                               1,
                                               0);

                // Orphan the PBO. This is the recommended way to hint to the
                // driver to detach the underlying storage from this PBO id.
                // Keeping the size the same gives the driver a hint for future
                // use of this PBO.
                device.orphan_pbo(mem::size_of::<GpuBlockData>() * MAX_VERTEX_TEXTURE_WIDTH);

                row.is_dirty = false;
            }
        }

        // Ensure that other texture updates won't read from this PBO.
        device.bind_pbo(None);
    }
}


trait GpuStoreLayout {
    fn image_format() -> ImageFormat;

    fn texture_width<T>() -> usize;

    fn texture_filter() -> TextureFilter;

    fn texel_size() -> usize {
        match Self::image_format() {
            ImageFormat::BGRA8 => 4,
            ImageFormat::RGBAF32 => 16,
            _ => unreachable!(),
        }
    }

    fn texels_per_item<T>() -> usize {
        let item_size = mem::size_of::<T>();
        let texel_size = Self::texel_size();
        debug_assert!(item_size % texel_size == 0);
        item_size / texel_size
    }

    fn items_per_row<T>() -> usize {
        Self::texture_width::<T>() / Self::texels_per_item::<T>()
    }

    fn rows_per_item<T>() -> usize {
        Self::texels_per_item::<T>() / Self::texture_width::<T>()
    }
}

struct GpuDataTexture<L> {
    id: TextureId,
    layout: PhantomData<L>,
}

impl<L: GpuStoreLayout> GpuDataTexture<L> {
    fn new(device: &mut Device) -> GpuDataTexture<L> {
        let id = device.create_texture_ids(1, TextureTarget::Default)[0];

        GpuDataTexture {
            id,
            layout: PhantomData,
        }
    }

    fn init<T: Default>(&mut self,
                        device: &mut Device,
                        data: &mut Vec<T>) {
        if data.is_empty() {
            return;
        }

        let items_per_row = L::items_per_row::<T>();
        let rows_per_item = L::rows_per_item::<T>();

        // Extend the data array to be a multiple of the row size.
        // This ensures memory safety when the array is passed to
        // OpenGL to upload to the GPU.
        if items_per_row != 0 {
            while data.len() % items_per_row != 0 {
                data.push(T::default());
            }
        }

        let height = if items_per_row != 0 {
            data.len() / items_per_row
        } else {
            data.len() * rows_per_item
        };

        device.init_texture(self.id,
                            L::texture_width::<T>() as u32,
                            height as u32,
                            L::image_format(),
                            L::texture_filter(),
                            RenderTargetMode::None,
                            Some(unsafe { mem::transmute(data.as_slice()) } ));
    }
}

pub struct VertexDataTextureLayout {}

impl GpuStoreLayout for VertexDataTextureLayout {
    fn image_format() -> ImageFormat {
        ImageFormat::RGBAF32
    }

    fn texture_width<T>() -> usize {
        MAX_VERTEX_TEXTURE_WIDTH - (MAX_VERTEX_TEXTURE_WIDTH % Self::texels_per_item::<T>())
    }

    fn texture_filter() -> TextureFilter {
        TextureFilter::Nearest
    }
}

type VertexDataTexture = GpuDataTexture<VertexDataTextureLayout>;

const TRANSFORM_FEATURE: &str = "TRANSFORM";
const SUBPIXEL_AA_FEATURE: &str = "SUBPIXEL_AA";
const CLIP_FEATURE: &str = "CLIP";

enum ShaderKind {
    Primitive,
    Cache(VertexFormat),
    ClipCache,
}

struct LazilyCompiledShader {
    program: Option<Program>,
    name: &'static str,
    kind: ShaderKind,
    features: Vec<&'static str>,
}

impl LazilyCompiledShader {
    fn new(kind: ShaderKind,
           name: &'static str,
           features: &[&'static str],
           device: &mut Device,
           precache: bool) -> Result<LazilyCompiledShader, ShaderError> {
        let mut shader = LazilyCompiledShader {
            program: None,
            name,
            kind,
            features: features.to_vec(),
        };

        if precache {
            try!{ shader.get(device) };
        }

        Ok(shader)
    }

    fn bind(&mut self, device: &mut Device, projection: &Transform3D<f32>) {
        let program = self.get(device)
                          .expect("Unable to get shader!");
        device.bind_program(program);
        device.set_uniforms(program, projection);
    }

    fn get(&mut self, device: &mut Device) -> Result<&Program, ShaderError> {
        if self.program.is_none() {
            let program = try!{
                match self.kind {
                    ShaderKind::Primitive => {
                        create_prim_shader(self.name,
                                           device,
                                           &self.features,
                                           VertexFormat::PrimitiveInstances)
                    }
                    ShaderKind::Cache(format) => {
                        create_prim_shader(self.name,
                                           device,
                                           &self.features,
                                           format)
                    }
                    ShaderKind::ClipCache => {
                        create_clip_shader(self.name, device)
                    }
                }
            };
            self.program = Some(program);
        }

        Ok(self.program.as_ref().unwrap())
    }

    fn deinit(&mut self, device: &mut Device) {
        if let &mut Some(ref mut program) = &mut self.program {
            device.delete_program(program);
        }
    }
}

struct PrimitiveShader {
    simple: LazilyCompiledShader,
    transform: LazilyCompiledShader,
}

struct FileWatcher {
    notifier: Arc<Mutex<Option<Box<RenderNotifier>>>>,
    result_tx: Sender<ResultMsg>,
}

impl FileWatcherHandler for FileWatcher {
    fn file_changed(&self, path: PathBuf) {
        self.result_tx.send(ResultMsg::RefreshShader(path)).ok();
        let mut notifier = self.notifier.lock();
        notifier.as_mut().unwrap().as_mut().unwrap().new_frame_ready();
    }
}

impl PrimitiveShader {
    fn new(name: &'static str,
           device: &mut Device,
           features: &[&'static str],
           precache: bool) -> Result<PrimitiveShader, ShaderError> {
        let simple = try!{
            LazilyCompiledShader::new(ShaderKind::Primitive,
                                      name,
                                      features,
                                      device,
                                      precache)
        };

        let mut transform_features = features.to_vec();
        transform_features.push(TRANSFORM_FEATURE);

        let transform = try!{
            LazilyCompiledShader::new(ShaderKind::Primitive,
                                      name,
                                      &transform_features,
                                      device,
                                      precache)
        };

        Ok(PrimitiveShader {
            simple,
            transform,
        })
    }

    fn bind(&mut self,
            device: &mut Device,
            transform_kind: TransformedRectKind,
            projection: &Transform3D<f32>) {
        match transform_kind {
            TransformedRectKind::AxisAligned => self.simple.bind(device, projection),
            TransformedRectKind::Complex => self.transform.bind(device, projection),
        }
    }

    fn deinit(&mut self, device: &mut Device) {
        self.simple.deinit(device);
        self.transform.deinit(device);
    }
}

fn create_prim_shader(name: &'static str,
                      device: &mut Device,
                      features: &[&'static str],
                      vertex_format: VertexFormat) -> Result<Program, ShaderError> {
    let mut prefix = format!("#define WR_MAX_VERTEX_TEXTURE_WIDTH {}\n",
                              MAX_VERTEX_TEXTURE_WIDTH);

    for feature in features {
        prefix.push_str(&format!("#define WR_FEATURE_{}\n", feature));
    }

    debug!("PrimShader {}", name);

    let includes = &["prim_shared"];

    let vertex_descriptor = match vertex_format {
        VertexFormat::PrimitiveInstances => DESC_PRIM_INSTANCES,
        VertexFormat::Blur => DESC_BLUR,
        VertexFormat::Clip => DESC_CLIP,
    };

    device.create_program_with_prefix(name,
                                      includes,
                                      Some(prefix),
                                      &vertex_descriptor)
}

fn create_clip_shader(name: &'static str, device: &mut Device) -> Result<Program, ShaderError> {
    let prefix = format!("#define WR_MAX_VERTEX_TEXTURE_WIDTH {}\n
                          #define WR_FEATURE_TRANSFORM",
                          MAX_VERTEX_TEXTURE_WIDTH);

    debug!("ClipShader {}", name);

    let includes = &["prim_shared", "clip_shared"];
    device.create_program_with_prefix(name, includes, Some(prefix), &DESC_CLIP)
}

struct GpuDataTextures {
    layer_texture: VertexDataTexture,
    render_task_texture: VertexDataTexture,
}

impl GpuDataTextures {
    fn new(device: &mut Device) -> GpuDataTextures {
        GpuDataTextures {
            layer_texture: VertexDataTexture::new(device),
            render_task_texture: VertexDataTexture::new(device),
        }
    }

    fn init_frame(&mut self, device: &mut Device, frame: &mut Frame) {
        self.layer_texture.init(device, &mut frame.layer_texture_data);
        self.render_task_texture.init(device, &mut frame.render_task_data);

        device.bind_texture(TextureSampler::Layers, self.layer_texture.id);
        device.bind_texture(TextureSampler::RenderTasks, self.render_task_texture.id);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReadPixelsFormat {
    Rgba8,
    Bgra8,
}

/// The renderer is responsible for submitting to the GPU the work prepared by the
/// RenderBackend.
pub struct Renderer {
    result_rx: Receiver<ResultMsg>,
    device: Device,
    pending_texture_updates: Vec<TextureUpdateList>,
    pending_gpu_cache_updates: Vec<GpuCacheUpdateList>,
    pending_shader_updates: Vec<PathBuf>,
    current_frame: Option<RendererFrame>,

    // These are "cache shaders". These shaders are used to
    // draw intermediate results to cache targets. The results
    // of these shaders are then used by the primitive shaders.
    cs_box_shadow: LazilyCompiledShader,
    cs_text_run: LazilyCompiledShader,
    cs_line: LazilyCompiledShader,
    cs_blur: LazilyCompiledShader,

    /// These are "cache clip shaders". These shaders are used to
    /// draw clip instances into the cached clip mask. The results
    /// of these shaders are also used by the primitive shaders.
    cs_clip_rectangle: LazilyCompiledShader,
    cs_clip_image: LazilyCompiledShader,
    cs_clip_border: LazilyCompiledShader,

    // The are "primitive shaders". These shaders draw and blend
    // final results on screen. They are aware of tile boundaries.
    // Most draw directly to the framebuffer, but some use inputs
    // from the cache shaders to draw. Specifically, the box
    // shadow primitive shader stretches the box shadow cache
    // output, and the cache_image shader blits the results of
    // a cache shader (e.g. blur) to the screen.
    ps_rectangle: PrimitiveShader,
    ps_rectangle_clip: PrimitiveShader,
    ps_text_run: PrimitiveShader,
    ps_text_run_subpixel: PrimitiveShader,
    ps_image: Vec<Option<PrimitiveShader>>,
    ps_yuv_image: Vec<Option<PrimitiveShader>>,
    ps_border_corner: PrimitiveShader,
    ps_border_edge: PrimitiveShader,
    ps_gradient: PrimitiveShader,
    ps_angle_gradient: PrimitiveShader,
    ps_radial_gradient: PrimitiveShader,
    ps_box_shadow: PrimitiveShader,
    ps_cache_image: PrimitiveShader,
    ps_line: PrimitiveShader,

    ps_blend: LazilyCompiledShader,
    ps_hw_composite: LazilyCompiledShader,
    ps_split_composite: LazilyCompiledShader,
    ps_composite: LazilyCompiledShader,

    notifier: Arc<Mutex<Option<Box<RenderNotifier>>>>,

    max_texture_size: u32,

    max_recorded_profiles: usize,
    clear_framebuffer: bool,
    clear_color: ColorF,
    enable_clear_scissor: bool,
    debug: DebugRenderer,
    debug_flags: DebugFlags,
    enable_batcher: bool,
    backend_profile_counters: BackendProfileCounters,
    profile_counters: RendererProfileCounters,
    profiler: Profiler,
    last_time: u64,

    color_render_targets: Vec<TextureId>,
    alpha_render_targets: Vec<TextureId>,

    gpu_profile: GpuProfiler<GpuProfileTag>,
    prim_vao_id: VAOId,
    blur_vao_id: VAOId,
    clip_vao_id: VAOId,

    gdt_index: usize,
    gpu_data_textures: [GpuDataTextures; GPU_DATA_TEXTURE_POOL],

    gpu_cache_texture: CacheTexture,

    pipeline_epoch_map: FastHashMap<PipelineId, Epoch>,
    /// Used to dispatch functions to the main thread's event loop.
    /// Required to allow GLContext sharing in some implementations like WGL.
    main_thread_dispatcher: Arc<Mutex<Option<Box<RenderDispatcher>>>>,

    /// A vector for fast resolves of texture cache IDs to
    /// native texture IDs. This maps to a free-list managed
    /// by the backend thread / texture cache. We free the
    /// texture memory associated with a TextureId when its
    /// texture cache ID is freed by the texture cache, but
    /// reuse the TextureId when the texture caches's free
    /// list reuses the texture cache ID. This saves having to
    /// use a hashmap, and allows a flat vector for performance.
    cache_texture_id_map: Vec<TextureId>,

    /// A special 1x1 dummy cache texture used for shaders that expect to work
    /// with the cache but are actually running in the first pass
    /// when no target is yet provided as a cache texture input.
    dummy_cache_texture_id: TextureId,

    dither_matrix_texture_id: Option<TextureId>,

    /// Optional trait object that allows the client
    /// application to provide external buffers for image data.
    external_image_handler: Option<Box<ExternalImageHandler>>,

    /// Map of external image IDs to native textures.
    external_images: FastHashMap<(ExternalImageId, u8), TextureId>,

    // Optional trait object that handles WebVR commands.
    // Some WebVR commands such as SubmitFrame must be synced with the WebGL render thread.
    vr_compositor_handler: Arc<Mutex<Option<Box<VRCompositorHandler>>>>,

    /// List of profile results from previous frames. Can be retrieved
    /// via get_frame_profiles().
    cpu_profiles: VecDeque<CpuProfile>,
    gpu_profiles: VecDeque<GpuProfile>,
}

#[derive(Debug)]
pub enum InitError {
    Shader(ShaderError),
    Thread(std::io::Error),
    MaxTextureSize,
}

impl From<ShaderError> for InitError {
    fn from(err: ShaderError) -> Self { InitError::Shader(err) }
}

impl From<std::io::Error> for InitError {
    fn from(err: std::io::Error) -> Self { InitError::Thread(err) }
}

impl Renderer {
    /// Initializes webrender and creates a `Renderer` and `RenderApiSender`.
    ///
    /// # Examples
    /// Initializes a `Renderer` with some reasonable values. For more information see
    /// [`RendererOptions`][rendereroptions].
    ///
    /// ```rust,ignore
    /// # use webrender::renderer::Renderer;
    /// # use std::path::PathBuf;
    /// let opts = webrender::RendererOptions {
    ///    device_pixel_ratio: 1.0,
    ///    resource_override_path: None,
    ///    enable_aa: false,
    /// };
    /// let (renderer, sender) = Renderer::new(opts);
    /// ```
    /// [rendereroptions]: struct.RendererOptions.html
    pub fn new(gl: Rc<gl::Gl>, mut options: RendererOptions) -> Result<(Renderer, RenderApiSender), InitError> {

        let (api_tx, api_rx) = try!{ channel::msg_channel() };
        let (payload_tx, payload_rx) = try!{ channel::payload_channel() };
        let (result_tx, result_rx) = channel();
        let gl_type = gl.get_type();

        let notifier = Arc::new(Mutex::new(None));

        let file_watch_handler = FileWatcher {
            result_tx: result_tx.clone(),
            notifier: Arc::clone(&notifier),
        };

        let mut device = Device::new(
            gl,
            options.resource_override_path.clone(),
            Box::new(file_watch_handler)
        );

        let device_max_size = device.max_texture_size();
        // 512 is the minimum that the texture cache can work with.
        // Broken GL contexts can return a max texture size of zero (See #1260). Better to
        // gracefully fail now than panic as soon as a texture is allocated.
        let min_texture_size = 512;
        if device_max_size < min_texture_size {
            println!("Device reporting insufficient max texture size ({})", device_max_size);
            return Err(InitError::MaxTextureSize);
        }
        let max_device_size = cmp::max(
            cmp::min(device_max_size, options.max_texture_size.unwrap_or(device_max_size)),
            min_texture_size
        );

        register_thread_with_profiler("Compositor".to_owned());

        // device-pixel ratio doesn't matter here - we are just creating resources.
        device.begin_frame(1.0);

        let cs_box_shadow = try!{
            LazilyCompiledShader::new(ShaderKind::Cache(VertexFormat::PrimitiveInstances),
                                      "cs_box_shadow",
                                      &[],
                                      &mut device,
                                      options.precache_shaders)
        };

        let cs_text_run = try!{
            LazilyCompiledShader::new(ShaderKind::Cache(VertexFormat::PrimitiveInstances),
                                      "cs_text_run",
                                      &[],
                                      &mut device,
                                      options.precache_shaders)
        };

        let cs_line = try!{
            LazilyCompiledShader::new(ShaderKind::Cache(VertexFormat::PrimitiveInstances),
                                      "ps_line",
                                      &["CACHE"],
                                      &mut device,
                                      options.precache_shaders)
        };

        let cs_blur = try!{
            LazilyCompiledShader::new(ShaderKind::Cache(VertexFormat::Blur),
                                     "cs_blur",
                                      &[],
                                      &mut device,
                                      options.precache_shaders)
        };

        let cs_clip_rectangle = try!{
            LazilyCompiledShader::new(ShaderKind::ClipCache,
                                      "cs_clip_rectangle",
                                      &[],
                                      &mut device,
                                      options.precache_shaders)
        };

        let cs_clip_image = try!{
            LazilyCompiledShader::new(ShaderKind::ClipCache,
                                      "cs_clip_image",
                                      &[],
                                      &mut device,
                                      options.precache_shaders)
        };

        let cs_clip_border = try!{
            LazilyCompiledShader::new(ShaderKind::ClipCache,
                                      "cs_clip_border",
                                      &[],
                                      &mut device,
                                      options.precache_shaders)
        };

        let ps_rectangle = try!{
            PrimitiveShader::new("ps_rectangle",
                                 &mut device,
                                 &[],
                                 options.precache_shaders)
        };

        let ps_rectangle_clip = try!{
            PrimitiveShader::new("ps_rectangle",
                                 &mut device,
                                 &[ CLIP_FEATURE ],
                                 options.precache_shaders)
        };

        let ps_line = try!{
            PrimitiveShader::new("ps_line",
                                 &mut device,
                                 &[],
                                 options.precache_shaders)
        };

        let ps_text_run = try!{
            PrimitiveShader::new("ps_text_run",
                                 &mut device,
                                 &[],
                                 options.precache_shaders)
        };

        let ps_text_run_subpixel = try!{
            PrimitiveShader::new("ps_text_run",
                                 &mut device,
                                 &[ SUBPIXEL_AA_FEATURE ],
                                 options.precache_shaders)
        };

        // All image configuration.
        let mut image_features = Vec::new();
        let mut ps_image: Vec<Option<PrimitiveShader>> = Vec::new();
        // PrimitiveShader is not clonable. Use push() to initialize the vec.
        for _ in 0..IMAGE_BUFFER_KINDS.len() {
            ps_image.push(None);
        }
        for buffer_kind in 0..IMAGE_BUFFER_KINDS.len() {
            if IMAGE_BUFFER_KINDS[buffer_kind].has_platform_support(&gl_type) {
                let feature_string = IMAGE_BUFFER_KINDS[buffer_kind].get_feature_string();
                if feature_string != "" {
                    image_features.push(feature_string);
                }
                let shader = try!{
                    PrimitiveShader::new("ps_image",
                                         &mut device,
                                         &image_features,
                                         options.precache_shaders)
                };
                ps_image[buffer_kind] = Some(shader);
            }
            image_features.clear();
        }

        // All yuv_image configuration.
        let mut yuv_features = Vec::new();
        let yuv_shader_num = IMAGE_BUFFER_KINDS.len() *
                             YUV_FORMATS.len() *
                             YUV_COLOR_SPACES.len();
        let mut ps_yuv_image: Vec<Option<PrimitiveShader>> = Vec::new();
        // PrimitiveShader is not clonable. Use push() to initialize the vec.
        for _ in 0..yuv_shader_num {
            ps_yuv_image.push(None);
        }
        for buffer_kind in 0..IMAGE_BUFFER_KINDS.len() {
            if IMAGE_BUFFER_KINDS[buffer_kind].has_platform_support(&gl_type) {
                for format_kind in 0..YUV_FORMATS.len() {
                    for color_space_kind in 0..YUV_COLOR_SPACES.len() {
                        let feature_string = IMAGE_BUFFER_KINDS[buffer_kind].get_feature_string();
                        if feature_string != "" {
                            yuv_features.push(feature_string);
                        }
                        let feature_string = YUV_FORMATS[format_kind].get_feature_string();
                        if feature_string != "" {
                            yuv_features.push(feature_string);
                        }
                        let feature_string = YUV_COLOR_SPACES[color_space_kind].get_feature_string();
                        if feature_string != "" {
                            yuv_features.push(feature_string);
                        }

                        let shader = try!{
                            PrimitiveShader::new("ps_yuv_image",
                                                 &mut device,
                                                 &yuv_features,
                                                 options.precache_shaders)
                        };
                        let index = Renderer::get_yuv_shader_index(IMAGE_BUFFER_KINDS[buffer_kind],
                                                                   YUV_FORMATS[format_kind],
                                                                   YUV_COLOR_SPACES[color_space_kind]);
                        ps_yuv_image[index] = Some(shader);
                        yuv_features.clear();
                    }
                }
            }
        }

        let ps_border_corner = try!{
            PrimitiveShader::new("ps_border_corner",
                                 &mut device,
                                 &[],
                                 options.precache_shaders)
        };

        let ps_border_edge = try!{
            PrimitiveShader::new("ps_border_edge",
                                 &mut device,
                                 &[],
                                 options.precache_shaders)
        };

        let ps_box_shadow = try!{
            PrimitiveShader::new("ps_box_shadow",
                                 &mut device,
                                 &[],
                                 options.precache_shaders)
        };

        let dithering_feature = ["DITHERING"];

        let ps_gradient = try!{
            PrimitiveShader::new("ps_gradient",
                                 &mut device,
                                 if options.enable_dithering {
                                    &dithering_feature
                                 } else {
                                    &[]
                                 },
                                 options.precache_shaders)
        };

        let ps_angle_gradient = try!{
            PrimitiveShader::new("ps_angle_gradient",
                                 &mut device,
                                 if options.enable_dithering {
                                    &dithering_feature
                                 } else {
                                    &[]
                                 },
                                 options.precache_shaders)
        };

        let ps_radial_gradient = try!{
            PrimitiveShader::new("ps_radial_gradient",
                                 &mut device,
                                 if options.enable_dithering {
                                    &dithering_feature
                                 } else {
                                    &[]
                                 },
                                 options.precache_shaders)
        };

        let ps_cache_image = try!{
            PrimitiveShader::new("ps_cache_image",
                                 &mut device,
                                 &[],
                                 options.precache_shaders)
        };

        let ps_blend = try!{
            LazilyCompiledShader::new(ShaderKind::Primitive,
                                     "ps_blend",
                                     &[],
                                     &mut device,
                                     options.precache_shaders)
        };

        let ps_composite = try!{
            LazilyCompiledShader::new(ShaderKind::Primitive,
                                      "ps_composite",
                                      &[],
                                      &mut device,
                                      options.precache_shaders)
        };

        let ps_hw_composite = try!{
            LazilyCompiledShader::new(ShaderKind::Primitive,
                                     "ps_hardware_composite",
                                     &[],
                                     &mut device,
                                     options.precache_shaders)
        };

        let ps_split_composite = try!{
            LazilyCompiledShader::new(ShaderKind::Primitive,
                                     "ps_split_composite",
                                     &[],
                                     &mut device,
                                     options.precache_shaders)
        };

        let texture_cache = TextureCache::new(max_device_size);
        let max_texture_size = texture_cache.max_texture_size();

        let backend_profile_counters = BackendProfileCounters::new();

        let dummy_cache_texture_id = device.create_texture_ids(1, TextureTarget::Array)[0];
        device.init_texture(dummy_cache_texture_id,
                            1,
                            1,
                            ImageFormat::BGRA8,
                            TextureFilter::Linear,
                            RenderTargetMode::LayerRenderTarget(1),
                            None);

        let dither_matrix_texture_id = if options.enable_dithering {
            let dither_matrix: [u8; 64] = [
                00, 48, 12, 60, 03, 51, 15, 63,
                32, 16, 44, 28, 35, 19, 47, 31,
                08, 56, 04, 52, 11, 59, 07, 55,
                40, 24, 36, 20, 43, 27, 39, 23,
                02, 50, 14, 62, 01, 49, 13, 61,
                34, 18, 46, 30, 33, 17, 45, 29,
                10, 58, 06, 54, 09, 57, 05, 53,
                42, 26, 38, 22, 41, 25, 37, 21
            ];

            let id = device.create_texture_ids(1, TextureTarget::Default)[0];
            device.init_texture(id,
                                8,
                                8,
                                ImageFormat::A8,
                                TextureFilter::Nearest,
                                RenderTargetMode::None,
                                Some(&dither_matrix));

            Some(id)
        } else {
            None
        };

        let debug_renderer = DebugRenderer::new(&mut device);

        let gpu_data_textures = [
            GpuDataTextures::new(&mut device),
            GpuDataTextures::new(&mut device),
            GpuDataTextures::new(&mut device),
            GpuDataTextures::new(&mut device),
            GpuDataTextures::new(&mut device),
        ];

        let x0 = 0.0;
        let y0 = 0.0;
        let x1 = 1.0;
        let y1 = 1.0;

        let quad_indices: [u16; 6] = [ 0, 1, 2, 2, 1, 3 ];
        let quad_vertices = [
            PackedVertex {
                pos: [x0, y0],
            },
            PackedVertex {
                pos: [x1, y0],
            },
            PackedVertex {
                pos: [x0, y1],
            },
            PackedVertex {
                pos: [x1, y1],
            },
        ];

        let prim_vao_id = device.create_vao(&DESC_PRIM_INSTANCES, mem::size_of::<PrimitiveInstance>() as i32);
        device.bind_vao(prim_vao_id);
        device.update_vao_indices(prim_vao_id, &quad_indices, VertexUsageHint::Static);
        device.update_vao_main_vertices(prim_vao_id, &quad_vertices, VertexUsageHint::Static);

        let blur_vao_id = device.create_vao_with_new_instances(&DESC_BLUR, mem::size_of::<BlurCommand>() as i32, prim_vao_id);
        let clip_vao_id = device.create_vao_with_new_instances(&DESC_CLIP, mem::size_of::<CacheClipInstance>() as i32, prim_vao_id);

        device.end_frame();

        let main_thread_dispatcher = Arc::new(Mutex::new(None));
        let backend_notifier = Arc::clone(&notifier);
        let backend_main_thread_dispatcher = Arc::clone(&main_thread_dispatcher);

        let vr_compositor = Arc::new(Mutex::new(None));
        let backend_vr_compositor = Arc::clone(&vr_compositor);

        // We need a reference to the webrender context from the render backend in order to share
        // texture ids
        let context_handle = match options.renderer_kind {
            RendererKind::Native => GLContextHandleWrapper::current_native_handle(),
            RendererKind::OSMesa => GLContextHandleWrapper::current_osmesa_handle(),
        };

        let default_font_render_mode = match (options.enable_aa, options.enable_subpixel_aa) {
            (true, true) => FontRenderMode::Subpixel,
            (true, false) => FontRenderMode::Alpha,
            (false, _) => FontRenderMode::Mono,
        };

        let config = FrameBuilderConfig {
            enable_scrollbars: options.enable_scrollbars,
            default_font_render_mode,
            debug: options.debug,
            cache_expiry_frames: options.cache_expiry_frames,
        };

        let device_pixel_ratio = options.device_pixel_ratio;
        let debug_flags = options.debug_flags;
        let payload_tx_for_backend = payload_tx.clone();
        let recorder = options.recorder;
        let worker_config = ThreadPoolConfig::new()
            .thread_name(|idx|{ format!("WebRender:Worker#{}", idx) })
            .start_handler(|idx| { register_thread_with_profiler(format!("WebRender:Worker#{}", idx)); });
        let workers = options.workers.take().unwrap_or_else(||{
            Arc::new(ThreadPool::new(worker_config).unwrap())
        });
        let enable_render_on_scroll = options.enable_render_on_scroll;

        let blob_image_renderer = options.blob_image_renderer.take();
        try!{ thread::Builder::new().name("RenderBackend".to_string()).spawn(move || {
            let mut backend = RenderBackend::new(api_rx,
                                                 payload_rx,
                                                 payload_tx_for_backend,
                                                 result_tx,
                                                 device_pixel_ratio,
                                                 texture_cache,
                                                 workers,
                                                 backend_notifier,
                                                 context_handle,
                                                 config,
                                                 recorder,
                                                 backend_main_thread_dispatcher,
                                                 blob_image_renderer,
                                                 backend_vr_compositor,
                                                 enable_render_on_scroll);
            backend.run(backend_profile_counters);
        })};

        let gpu_cache_texture = CacheTexture::new(&mut device);

        let gpu_profile = GpuProfiler::new(device.rc_gl());

        let renderer = Renderer {
            result_rx,
            device,
            current_frame: None,
            pending_texture_updates: Vec::new(),
            pending_gpu_cache_updates: Vec::new(),
            pending_shader_updates: Vec::new(),
            cs_box_shadow,
            cs_text_run,
            cs_line,
            cs_blur,
            cs_clip_rectangle,
            cs_clip_border,
            cs_clip_image,
            ps_rectangle,
            ps_rectangle_clip,
            ps_text_run,
            ps_text_run_subpixel,
            ps_image,
            ps_yuv_image,
            ps_border_corner,
            ps_border_edge,
            ps_box_shadow,
            ps_gradient,
            ps_angle_gradient,
            ps_radial_gradient,
            ps_cache_image,
            ps_blend,
            ps_hw_composite,
            ps_split_composite,
            ps_composite,
            ps_line,
            notifier,
            debug: debug_renderer,
            debug_flags,
            enable_batcher: options.enable_batcher,
            backend_profile_counters: BackendProfileCounters::new(),
            profile_counters: RendererProfileCounters::new(),
            profiler: Profiler::new(),
            max_texture_size: max_texture_size,
            max_recorded_profiles: options.max_recorded_profiles,
            clear_framebuffer: options.clear_framebuffer,
            clear_color: options.clear_color,
            enable_clear_scissor: options.enable_clear_scissor,
            last_time: 0,
            color_render_targets: Vec::new(),
            alpha_render_targets: Vec::new(),
            gpu_profile,
            prim_vao_id,
            blur_vao_id,
            clip_vao_id,
            gdt_index: 0,
            gpu_data_textures,
            pipeline_epoch_map: FastHashMap::default(),
            main_thread_dispatcher,
            cache_texture_id_map: Vec::new(),
            dummy_cache_texture_id,
            dither_matrix_texture_id,
            external_image_handler: None,
            external_images: FastHashMap::default(),
            vr_compositor_handler: vr_compositor,
            cpu_profiles: VecDeque::new(),
            gpu_profiles: VecDeque::new(),
            gpu_cache_texture,
        };

        let sender = RenderApiSender::new(api_tx, payload_tx);
        Ok((renderer, sender))
    }

    pub fn get_max_texture_size(&self) -> u32 {
        self.max_texture_size
    }

    pub fn get_graphics_api_info(&self) -> GraphicsApiInfo {
        GraphicsApiInfo {
            kind: GraphicsApi::OpenGL,
            version: self.device.gl().get_string(gl::VERSION),
            renderer: self.device.gl().get_string(gl::RENDERER),
        }
    }

    fn get_yuv_shader_index(buffer_kind: ImageBufferKind, format: YuvFormat, color_space: YuvColorSpace) -> usize {
        ((buffer_kind as usize) * YUV_FORMATS.len() + (format as usize)) * YUV_COLOR_SPACES.len() + (color_space as usize)
    }

    /// Sets the new RenderNotifier.
    ///
    /// The RenderNotifier will be called when processing e.g. of a (scrolling) frame is done,
    /// and therefore the screen should be updated.
    pub fn set_render_notifier(&self, notifier: Box<RenderNotifier>) {
        let mut notifier_arc = self.notifier.lock().unwrap();
        *notifier_arc = Some(notifier);
    }

    /// Sets the new main thread dispatcher.
    ///
    /// Allows to dispatch functions to the main thread's event loop.
    pub fn set_main_thread_dispatcher(&self, dispatcher: Box<RenderDispatcher>) {
        let mut dispatcher_arc = self.main_thread_dispatcher.lock().unwrap();
        *dispatcher_arc = Some(dispatcher);
    }

    /// Sets the VRCompositorHandler.
    ///
    /// It's used to handle WebVR render commands.
    /// Some WebVR commands such as Vsync and SubmitFrame must be called in the WebGL render thread.
    pub fn set_vr_compositor_handler(&self, creator: Box<VRCompositorHandler>) {
        let mut handler_arc = self.vr_compositor_handler.lock().unwrap();
        *handler_arc = Some(creator);
    }

    /// Returns the Epoch of the current frame in a pipeline.
    pub fn current_epoch(&self, pipeline_id: PipelineId) -> Option<Epoch> {
        self.pipeline_epoch_map.get(&pipeline_id).cloned()
    }

    /// Returns a HashMap containing the pipeline ids that have been received by the renderer and
    /// their respective epochs since the last time the method was called.
    pub fn flush_rendered_epochs(&mut self) -> FastHashMap<PipelineId, Epoch> {
        mem::replace(&mut self.pipeline_epoch_map, FastHashMap::default())
    }

    /// Processes the result queue.
    ///
    /// Should be called before `render()`, as texture cache updates are done here.
    pub fn update(&mut self) {
        profile_scope!("update");

        // Pull any pending results and return the most recent.
        while let Ok(msg) = self.result_rx.try_recv() {
            match msg {
                ResultMsg::NewFrame(_document_id, mut frame, texture_update_list, profile_counters) => {
                    //TODO: associate `document_id` with target window
                    self.pending_texture_updates.push(texture_update_list);
                    if let Some(ref mut frame) = frame.frame {
                        // TODO(gw): This whole message / Frame / RendererFrame stuff
                        //           is really messy and needs to be refactored!!
                        if let Some(update_list) = frame.gpu_cache_updates.take() {
                            self.pending_gpu_cache_updates.push(update_list);
                        }
                    }
                    self.backend_profile_counters = profile_counters;

                    // Update the list of available epochs for use during reftests.
                    // This is a workaround for https://github.com/servo/servo/issues/13149.
                    for (pipeline_id, epoch) in &frame.pipeline_epoch_map {
                        self.pipeline_epoch_map.insert(*pipeline_id, *epoch);
                    }

                    self.current_frame = Some(frame);
                }
                ResultMsg::UpdateResources { updates, cancel_rendering } => {
                    self.pending_texture_updates.push(updates);
                    self.update_texture_cache();
                    // If we receive a NewFrame message followed by this one within
                    // the same update we need ot cancel the frame because we might
                    // have deleted the resources in use in the frame dut to a memory
                    // pressure event.
                    if cancel_rendering {
                        self.current_frame = None;
                    }
                }
                ResultMsg::RefreshShader(path) => {
                    self.pending_shader_updates.push(path);
                }
            }
        }
    }

    // Get the real (OpenGL) texture ID for a given source texture.
    // For a texture cache texture, the IDs are stored in a vector
    // map for fast access. For WebGL textures, the native texture ID
    // is stored inline. When we add support for external textures,
    // we will add a callback here that is able to ask the caller
    // for the image data.
    fn resolve_source_texture(&mut self, texture_id: &SourceTexture) -> TextureId {
        match *texture_id {
            SourceTexture::Invalid => TextureId::invalid(),
            SourceTexture::WebGL(id) => TextureId::new(id, TextureTarget::Default),
            SourceTexture::External(external_image) => {
                *self.external_images
                     .get(&(external_image.id, external_image.channel_index))
                     .expect("BUG: External image should be resolved by now!")
            }
            SourceTexture::TextureCache(index) => {
                self.cache_texture_id_map[index.0]
            }
        }
    }

    /// Set a callback for handling external images.
    pub fn set_external_image_handler(&mut self, handler: Box<ExternalImageHandler>) {
        self.external_image_handler = Some(handler);
    }

    /// Retrieve (and clear) the current list of recorded frame profiles.
    pub fn get_frame_profiles(&mut self) -> (Vec<CpuProfile>, Vec<GpuProfile>) {
        let cpu_profiles = self.cpu_profiles.drain(..).collect();
        let gpu_profiles = self.gpu_profiles.drain(..).collect();
        (cpu_profiles, gpu_profiles)
    }

    /// Renders the current frame.
    ///
    /// A Frame is supplied by calling [`generate_frame()`][genframe].
    /// [genframe]: ../../webrender_api/struct.DocumentApi.html#method.generate_frame
    pub fn render(&mut self, framebuffer_size: DeviceUintSize) {
        profile_scope!("render");

        if let Some(mut frame) = self.current_frame.take() {
            if let Some(ref mut frame) = frame.frame {
                let mut profile_timers = RendererProfileTimers::new();

                {
                    //Note: avoiding `self.gpu_profile.add_marker` - it would block here
                    let _gm = GpuMarker::new(self.device.rc_gl(), "build samples");
                    // Block CPU waiting for last frame's GPU profiles to arrive.
                    // In general this shouldn't block unless heavily GPU limited.
                    if let Some((gpu_frame_id, samples)) = self.gpu_profile.build_samples() {
                        if self.max_recorded_profiles > 0 {
                            while self.gpu_profiles.len() >= self.max_recorded_profiles {
                                self.gpu_profiles.pop_front();
                            }
                            self.gpu_profiles.push_back(GpuProfile::new(gpu_frame_id, &samples));
                        }
                        profile_timers.gpu_samples = samples;
                    }
                }

                let cpu_frame_id = profile_timers.cpu_time.profile(|| {
                    let cpu_frame_id = {
                        let _gm = GpuMarker::new(self.device.rc_gl(), "begin frame");
                        let frame_id = self.device.begin_frame(frame.device_pixel_ratio);
                        self.gpu_profile.begin_frame(frame_id);

                        self.device.disable_scissor();
                        self.device.disable_depth();
                        self.device.set_blend(false);
                        //self.update_shaders();

                        self.update_texture_cache();

                        self.update_gpu_cache(frame);

                        self.device.bind_texture(TextureSampler::ResourceCache, self.gpu_cache_texture.texture_id);

                        frame_id
                    };

                    self.draw_tile_frame(frame, &framebuffer_size);

                    self.gpu_profile.end_frame();
                    cpu_frame_id
                });

                let current_time = precise_time_ns();
                let ns = current_time - self.last_time;
                self.profile_counters.frame_time.set(ns);

                if self.max_recorded_profiles > 0 {
                    while self.cpu_profiles.len() >= self.max_recorded_profiles {
                        self.cpu_profiles.pop_front();
                    }
                    let cpu_profile = CpuProfile::new(cpu_frame_id,
                                                      self.backend_profile_counters.total_time.get(),
                                                      profile_timers.cpu_time.get(),
                                                      self.profile_counters.draw_calls.get());
                    self.cpu_profiles.push_back(cpu_profile);
                }

                if self.debug_flags.contains(PROFILER_DBG) {
                    self.profiler.draw_profile(&mut self.device,
                                               &frame.profile_counters,
                                               &self.backend_profile_counters,
                                               &self.profile_counters,
                                               &mut profile_timers,
                                               &mut self.debug);
                }

                self.profile_counters.reset();
                self.profile_counters.frame_counter.inc();

                let debug_size = DeviceUintSize::new(framebuffer_size.width as u32,
                                                     framebuffer_size.height as u32);
                self.debug.render(&mut self.device, &debug_size);
                {
                    let _gm = GpuMarker::new(self.device.rc_gl(), "end frame");
                    self.device.end_frame();
                }
                self.last_time = current_time;
            }

            // Restore frame - avoid borrow checker!
            self.current_frame = Some(frame);
        }
    }

    pub fn layers_are_bouncing_back(&self) -> bool {
        match self.current_frame {
            None => false,
            Some(ref current_frame) => !current_frame.layers_bouncing_back.is_empty(),
        }
    }

/*
    fn update_shaders(&mut self) {
        let update_uniforms = !self.pending_shader_updates.is_empty();

        for path in self.pending_shader_updates.drain(..) {
            panic!("todo");
            //self.device.refresh_shader(path);
        }

        if update_uniforms {
            self.update_uniform_locations();
        }
    }
*/

    fn update_gpu_cache(&mut self, frame: &mut Frame) {
        let _gm = GpuMarker::new(self.device.rc_gl(), "gpu cache update");
        for update_list in self.pending_gpu_cache_updates.drain(..) {
            self.gpu_cache_texture.update(&mut self.device, &update_list);
        }
        self.update_deferred_resolves(frame);
        self.gpu_cache_texture.flush(&mut self.device);
    }

    fn update_texture_cache(&mut self) {
        let _gm = GpuMarker::new(self.device.rc_gl(), "texture cache update");
        let mut pending_texture_updates = mem::replace(&mut self.pending_texture_updates, vec![]);
        for update_list in pending_texture_updates.drain(..) {
            for update in update_list.updates {
                match update.op {
                    TextureUpdateOp::Create { width, height, format, filter, mode, data } => {
                        let CacheTextureId(cache_texture_index) = update.id;
                        if self.cache_texture_id_map.len() == cache_texture_index {
                            // Create a new native texture, as requested by the texture cache.
                            let texture_id = self.device
                                                 .create_texture_ids(1, TextureTarget::Default)[0];
                            self.cache_texture_id_map.push(texture_id);
                        }
                        let texture_id = self.cache_texture_id_map[cache_texture_index];

                        if let Some(image) = data {
                            match image {
                                ImageData::Raw(raw) => {
                                    self.device.init_texture(texture_id,
                                                             width,
                                                             height,
                                                             format,
                                                             filter,
                                                             mode,
                                                             Some(raw.as_slice()));
                                }
                                ImageData::External(ext_image) => {
                                    match ext_image.image_type {
                                        ExternalImageType::ExternalBuffer => {
                                            let handler = self.external_image_handler
                                                              .as_mut()
                                                              .expect("Found external image, but no handler set!");

                                            match handler.lock(ext_image.id, ext_image.channel_index).source {
                                                ExternalImageSource::RawData(raw) => {
                                                    self.device.init_texture(texture_id,
                                                                             width,
                                                                             height,
                                                                             format,
                                                                             filter,
                                                                             mode,
                                                                             Some(raw));
                                                }
                                                _ => panic!("No external buffer found"),
                                            };
                                            handler.unlock(ext_image.id, ext_image.channel_index);
                                        }
                                        ExternalImageType::Texture2DHandle |
                                        ExternalImageType::TextureRectHandle |
                                        ExternalImageType::TextureExternalHandle => {
                                            panic!("External texture handle should not use TextureUpdateOp::Create.");
                                        }
                                    }
                                }
                                _ => {
                                    panic!("No suitable image buffer for TextureUpdateOp::Create.");
                                }
                            }
                        } else {
                            self.device.init_texture(texture_id,
                                                     width,
                                                     height,
                                                     format,
                                                     filter,
                                                     mode,
                                                     None);
                        }
                    }
                    TextureUpdateOp::Grow { width, height, format, filter, mode } => {
                        let texture_id = self.cache_texture_id_map[update.id.0];
                        self.device.resize_texture(texture_id,
                                                   width,
                                                   height,
                                                   format,
                                                   filter,
                                                   mode);
                    }
                    TextureUpdateOp::Update { page_pos_x, page_pos_y, width, height, data, stride, offset } => {
                        let texture_id = self.cache_texture_id_map[update.id.0];
                        self.device.update_texture(texture_id,
                                                   page_pos_x,
                                                   page_pos_y,
                                                   width, height, stride,
                                                   &data[offset as usize..]);
                    }
                    TextureUpdateOp::UpdateForExternalBuffer { rect, id, channel_index, stride, offset } => {
                        let handler = self.external_image_handler
                                          .as_mut()
                                          .expect("Found external image, but no handler set!");
                        let device = &mut self.device;
                        let cached_id = self.cache_texture_id_map[update.id.0];

                        match handler.lock(id, channel_index).source {
                            ExternalImageSource::RawData(data) => {
                                device.update_texture(cached_id,
                                                      rect.origin.x,
                                                      rect.origin.y,
                                                      rect.size.width,
                                                      rect.size.height,
                                                      stride,
                                                      &data[offset as usize..]);
                            }
                            _ => panic!("No external buffer found"),
                        };
                        handler.unlock(id, channel_index);
                    }
                    TextureUpdateOp::Free => {
                        let texture_id = self.cache_texture_id_map[update.id.0];
                        self.device.deinit_texture(texture_id);
                    }
                }
            }
        }
    }

    fn draw_instanced_batch<T>(&mut self,
                               data: &[T],
                               vao: VAOId,
                               textures: &BatchTextures) {
        self.device.bind_vao(vao);

        for i in 0..textures.colors.len() {
            let texture_id = self.resolve_source_texture(&textures.colors[i]);
            self.device.bind_texture(TextureSampler::color(i), texture_id);
        }

        // TODO: this probably isn't the best place for this.
        if let Some(id) = self.dither_matrix_texture_id {
            self.device.bind_texture(TextureSampler::Dither, id);
        }

        if self.enable_batcher {
            self.device.update_vao_instances(vao, data, VertexUsageHint::Stream);
            self.device.draw_indexed_triangles_instanced_u16(6, data.len() as i32);
            self.profile_counters.draw_calls.inc();
        } else {
            for i in 0 .. data.len() {
                self.device.update_vao_instances(vao, &data[i..i+1], VertexUsageHint::Stream);
                self.device.draw_triangles_u16(0, 6);
                self.profile_counters.draw_calls.inc();
            }
        }

        self.profile_counters.vertices.add(6 * data.len());
    }

    fn submit_batch(&mut self,
                    batch: &PrimitiveBatch,
                    projection: &Transform3D<f32>,
                    render_task_data: &[RenderTaskData],
                    cache_texture: TextureId,
                    render_target: Option<(TextureId, i32)>,
                    target_dimensions: DeviceUintSize) {
        let transform_kind = batch.key.flags.transform_kind();
        let needs_clipping = batch.key.flags.needs_clipping();
        debug_assert!(!needs_clipping ||
                      match batch.key.blend_mode {
                          BlendMode::Alpha |
                          BlendMode::PremultipliedAlpha |
                          BlendMode::Subpixel(..) => true,
                          BlendMode::None => false,
                      });

        let marker = match batch.key.kind {
            AlphaBatchKind::Composite => {
                self.ps_composite.bind(&mut self.device, projection);
                GPU_TAG_PRIM_COMPOSITE
            }
            AlphaBatchKind::HardwareComposite => {
                self.ps_hw_composite.bind(&mut self.device, projection);
                GPU_TAG_PRIM_HW_COMPOSITE
            }
            AlphaBatchKind::SplitComposite => {
                self.ps_split_composite.bind(&mut self.device, projection);
                GPU_TAG_PRIM_SPLIT_COMPOSITE
            }
            AlphaBatchKind::Blend => {
                self.ps_blend.bind(&mut self.device, projection);
                GPU_TAG_PRIM_BLEND
            }
            AlphaBatchKind::Rectangle => {
                if needs_clipping {
                    self.ps_rectangle_clip.bind(&mut self.device, transform_kind, projection);
                } else {
                    self.ps_rectangle.bind(&mut self.device, transform_kind, projection);
                }
                GPU_TAG_PRIM_RECT
            }
            AlphaBatchKind::Line => {
                self.ps_line.bind(&mut self.device, transform_kind, projection);
                GPU_TAG_PRIM_LINE
            }
            AlphaBatchKind::TextRun => {
                match batch.key.blend_mode {
                    BlendMode::Subpixel(..) => {
                        self.ps_text_run_subpixel.bind(&mut self.device, transform_kind, projection);
                    }
                    BlendMode::Alpha |
                    BlendMode::PremultipliedAlpha |
                    BlendMode::None => {
                        self.ps_text_run.bind(&mut self.device, transform_kind, projection);
                    }
                };
                GPU_TAG_PRIM_TEXT_RUN
            }
            AlphaBatchKind::Image(image_buffer_kind) => {
                self.ps_image[image_buffer_kind as usize]
                    .as_mut()
                    .expect("Unsupported image shader kind")
                    .bind(&mut self.device, transform_kind, projection);
                GPU_TAG_PRIM_IMAGE
            }
            AlphaBatchKind::YuvImage(image_buffer_kind, format, color_space) => {
                let shader_index = Renderer::get_yuv_shader_index(image_buffer_kind,
                                                                  format,
                                                                  color_space);
                self.ps_yuv_image[shader_index]
                    .as_mut()
                    .expect("Unsupported YUV shader kind")
                    .bind(&mut self.device, transform_kind, projection);
                GPU_TAG_PRIM_YUV_IMAGE
            }
            AlphaBatchKind::BorderCorner => {
                self.ps_border_corner.bind(&mut self.device, transform_kind, projection);
                GPU_TAG_PRIM_BORDER_CORNER
            }
            AlphaBatchKind::BorderEdge => {
                self.ps_border_edge.bind(&mut self.device, transform_kind, projection);
                GPU_TAG_PRIM_BORDER_EDGE
            }
            AlphaBatchKind::AlignedGradient => {
                self.ps_gradient.bind(&mut self.device, transform_kind, projection);
                GPU_TAG_PRIM_GRADIENT
            }
            AlphaBatchKind::AngleGradient => {
                self.ps_angle_gradient.bind(&mut self.device, transform_kind, projection);
                GPU_TAG_PRIM_ANGLE_GRADIENT
            }
            AlphaBatchKind::RadialGradient => {
                self.ps_radial_gradient.bind(&mut self.device, transform_kind, projection);
                GPU_TAG_PRIM_RADIAL_GRADIENT
            }
            AlphaBatchKind::BoxShadow => {
                self.ps_box_shadow.bind(&mut self.device, transform_kind, projection);
                GPU_TAG_PRIM_BOX_SHADOW
            }
            AlphaBatchKind::CacheImage => {
                self.ps_cache_image.bind(&mut self.device, transform_kind, projection);
                GPU_TAG_PRIM_CACHE_IMAGE
            }
        };

        // Handle special case readback for composites.
        if batch.key.kind == AlphaBatchKind::Composite {
            // composites can't be grouped together because
            // they may overlap and affect each other.
            debug_assert!(batch.instances.len() == 1);
            let instance = CompositePrimitiveInstance::from(&batch.instances[0]);

            // TODO(gw): This code branch is all a bit hacky. We rely
            // on pulling specific values from the render target data
            // and also cloning the single primitive instance to be
            // able to pass to draw_instanced_batch(). We should
            // think about a cleaner way to achieve this!

            // Before submitting the composite batch, do the
            // framebuffer readbacks that are needed for each
            // composite operation in this batch.
            let cache_texture_dimensions = self.device.get_texture_dimensions(cache_texture);

            let backdrop = &render_task_data[instance.task_index.0 as usize];
            let readback = &render_task_data[instance.backdrop_task_index.0 as usize];
            let source = &render_task_data[instance.src_task_index.0 as usize];

            // Bind the FBO to blit the backdrop to.
            // Called per-instance in case the layer (and therefore FBO)
            // changes. The device will skip the GL call if the requested
            // target is already bound.
            let cache_draw_target = (cache_texture, readback.data[4] as i32);
            self.device.bind_draw_target(Some(cache_draw_target), Some(cache_texture_dimensions));

            let src_x = backdrop.data[0] - backdrop.data[4] + source.data[4];
            let src_y = backdrop.data[1] - backdrop.data[5] + source.data[5];

            let dest_x = readback.data[0];
            let dest_y = readback.data[1];

            let width = readback.data[2];
            let height = readback.data[3];

            let mut src = DeviceIntRect::new(DeviceIntPoint::new(src_x as i32, src_y as i32),
                                             DeviceIntSize::new(width as i32, height as i32));
            let mut dest = DeviceIntRect::new(DeviceIntPoint::new(dest_x as i32, dest_y as i32),
                                              DeviceIntSize::new(width as i32, height as i32));

            // Need to invert the y coordinates and flip the image vertically when
            // reading back from the framebuffer.
            if render_target.is_none() {
                src.origin.y = target_dimensions.height as i32 - src.size.height - src.origin.y;
                dest.origin.y += dest.size.height;
                dest.size.height = -dest.size.height;
            }

            self.device.blit_render_target(render_target,
                                           Some(src),
                                           dest);

            // Restore draw target to current pass render target + layer.
            self.device.bind_draw_target(render_target, Some(target_dimensions));
        }

        let _gm = self.gpu_profile.add_marker(marker);
        let vao = self.prim_vao_id;
        self.draw_instanced_batch(&batch.instances,
                                  vao,
                                  &batch.key.textures);
    }

    fn draw_color_target(&mut self,
                         render_target: Option<(TextureId, i32)>,
                         target: &ColorRenderTarget,
                         target_size: DeviceUintSize,
                         color_cache_texture: TextureId,
                         clear_color: Option<[f32; 4]>,
                         render_task_data: &[RenderTaskData],
                         projection: &Transform3D<f32>) {
        {
            let _gm = self.gpu_profile.add_marker(GPU_TAG_SETUP_TARGET);
            self.device.bind_draw_target(render_target, Some(target_size));
            self.device.disable_depth();
            self.device.enable_depth_write();
            self.device.set_blend(false);
            self.device.set_blend_mode_alpha();
            match render_target {
                Some(..) if self.enable_clear_scissor => {
                    // TODO(gw): Applying a scissor rect and minimal clear here
                    // is a very large performance win on the Intel and nVidia
                    // GPUs that I have tested with. It's possible it may be a
                    // performance penalty on other GPU types - we should test this
                    // and consider different code paths.
                    self.device.clear_target_rect(clear_color,
                                                  Some(1.0),
                                                  target.used_rect());
                }
                _ => {
                    self.device.clear_target(clear_color, Some(1.0));
                }
            }

            self.device.disable_depth_write();
        }

        // Draw any blurs for this target.
        // Blurs are rendered as a standard 2-pass
        // separable implementation.
        // TODO(gw): In the future, consider having
        //           fast path blur shaders for common
        //           blur radii with fixed weights.
        if !target.vertical_blurs.is_empty() || !target.horizontal_blurs.is_empty() {
            let _gm = self.gpu_profile.add_marker(GPU_TAG_BLUR);
            let vao = self.blur_vao_id;

            self.device.set_blend(false);
            self.cs_blur.bind(&mut self.device, projection);

            if !target.vertical_blurs.is_empty() {
                self.draw_instanced_batch(&target.vertical_blurs,
                                          vao,
                                          &BatchTextures::no_texture());
            }

            if !target.horizontal_blurs.is_empty() {
                self.draw_instanced_batch(&target.horizontal_blurs,
                                          vao,
                                          &BatchTextures::no_texture());
            }
        }

        // Draw any box-shadow caches for this target.
        if !target.box_shadow_cache_prims.is_empty() {
            self.device.set_blend(false);
            let _gm = self.gpu_profile.add_marker(GPU_TAG_CACHE_BOX_SHADOW);
            let vao = self.prim_vao_id;
            self.cs_box_shadow.bind(&mut self.device, projection);
            self.draw_instanced_batch(&target.box_shadow_cache_prims,
                                      vao,
                                      &BatchTextures::no_texture());
        }

        // Draw any textrun caches for this target. For now, this
        // is only used to cache text runs that are to be blurred
        // for text-shadow support. In the future it may be worth
        // considering using this for (some) other text runs, since
        // it removes the overhead of submitting many small glyphs
        // to multiple tiles in the normal text run case.
        if !target.text_run_cache_prims.is_empty() {
            self.device.set_blend(true);
            self.device.set_blend_mode_alpha();

            let _gm = self.gpu_profile.add_marker(GPU_TAG_CACHE_TEXT_RUN);
            let vao = self.prim_vao_id;
            self.cs_text_run.bind(&mut self.device, projection);
            self.draw_instanced_batch(&target.text_run_cache_prims,
                                      vao,
                                      &target.text_run_textures);
        }
        if !target.line_cache_prims.is_empty() {
            // TODO(gw): Technically, we don't need blend for solid
            //           lines. We could check that here?
            self.device.set_blend(true);
            self.device.set_blend_mode_alpha();

            let _gm = self.gpu_profile.add_marker(GPU_TAG_CACHE_LINE);
            let vao = self.prim_vao_id;
            self.cs_line.bind(&mut self.device, projection);
            self.draw_instanced_batch(&target.line_cache_prims,
                                      vao,
                                      &BatchTextures::no_texture());
        }

        if !target.alpha_batcher.is_empty() {
            let _gm2 = GpuMarker::new(self.device.rc_gl(), "alpha batches");
            self.device.set_blend(false);
            let mut prev_blend_mode = BlendMode::None;

            //Note: depth equality is needed for split planes
            self.device.set_depth_func(DepthFunction::LessEqual);
            self.device.enable_depth();
            self.device.enable_depth_write();

            // Draw opaque batches front-to-back for maximum
            // z-buffer efficiency!
            for batch in target.alpha_batcher
                               .batch_list
                               .opaque_batches
                               .iter()
                               .rev() {
                self.submit_batch(batch,
                                  &projection,
                                  render_task_data,
                                  color_cache_texture,
                                  render_target,
                                  target_size);
            }

            self.device.disable_depth_write();

            for batch in &target.alpha_batcher.batch_list.alpha_batches {
                if batch.key.blend_mode != prev_blend_mode {
                    match batch.key.blend_mode {
                        BlendMode::None => {
                            self.device.set_blend(false);
                        }
                        BlendMode::Alpha => {
                            self.device.set_blend(true);
                            self.device.set_blend_mode_alpha();
                        }
                        BlendMode::PremultipliedAlpha => {
                            self.device.set_blend(true);
                            self.device.set_blend_mode_premultiplied_alpha();
                        }
                        BlendMode::Subpixel(color) => {
                            self.device.set_blend(true);
                            self.device.set_blend_mode_subpixel(color);
                        }
                    }
                    prev_blend_mode = batch.key.blend_mode;
                }

                self.submit_batch(batch,
                                  &projection,
                                  render_task_data,
                                  color_cache_texture,
                                  render_target,
                                  target_size);
            }

            self.device.disable_depth();
            self.device.set_blend(false);
        }
    }

    fn draw_alpha_target(&mut self,
                         render_target: (TextureId, i32),
                         target: &AlphaRenderTarget,
                         target_size: DeviceUintSize,
                         projection: &Transform3D<f32>) {
        {
            let _gm = self.gpu_profile.add_marker(GPU_TAG_SETUP_TARGET);
            self.device.bind_draw_target(Some(render_target), Some(target_size));
            self.device.disable_depth();
            self.device.disable_depth_write();

            // TODO(gw): Applying a scissor rect and minimal clear here
            // is a very large performance win on the Intel and nVidia
            // GPUs that I have tested with. It's possible it may be a
            // performance penalty on other GPU types - we should test this
            // and consider different code paths.
            let clear_color = [1.0, 1.0, 1.0, 0.0];
            self.device.clear_target_rect(Some(clear_color),
                                          None,
                                          target.used_rect());
        }

        // Draw the clip items into the tiled alpha mask.
        {
            let _gm = self.gpu_profile.add_marker(GPU_TAG_CACHE_CLIP);
            let vao = self.clip_vao_id;

            // If we have border corner clips, the first step is to clear out the
            // area in the clip mask. This allows drawing multiple invididual clip
            // in regions below.
            if !target.clip_batcher.border_clears.is_empty() {
                let _gm2 = GpuMarker::new(self.device.rc_gl(), "clip borders [clear]");
                self.device.set_blend(false);
                self.cs_clip_border.bind(&mut self.device, projection);
                self.draw_instanced_batch(&target.clip_batcher.border_clears,
                                          vao,
                                          &BatchTextures::no_texture());
            }

            // Draw any dots or dashes for border corners.
            if !target.clip_batcher.borders.is_empty() {
                let _gm2 = GpuMarker::new(self.device.rc_gl(), "clip borders");
                // We are masking in parts of the corner (dots or dashes) here.
                // Blend mode is set to max to allow drawing multiple dots.
                // The individual dots and dashes in a border never overlap, so using
                // a max blend mode here is fine.
                self.device.set_blend(true);
                self.device.set_blend_mode_max();
                self.cs_clip_border.bind(&mut self.device, projection);
                self.draw_instanced_batch(&target.clip_batcher.borders,
                                          vao,
                                          &BatchTextures::no_texture());
            }

            // switch to multiplicative blending
            self.device.set_blend(true);
            self.device.set_blend_mode_multiply();

            // draw rounded cornered rectangles
            if !target.clip_batcher.rectangles.is_empty() {
                let _gm2 = GpuMarker::new(self.device.rc_gl(), "clip rectangles");
                self.cs_clip_rectangle.bind(&mut self.device, projection);
                self.draw_instanced_batch(&target.clip_batcher.rectangles,
                                          vao,
                                          &BatchTextures::no_texture());
            }
            // draw image masks
            for (mask_texture_id, items) in target.clip_batcher.images.iter() {
                let _gm2 = GpuMarker::new(self.device.rc_gl(), "clip images");
                let textures = BatchTextures {
                    colors: [
                        mask_texture_id.clone(),
                        SourceTexture::Invalid,
                        SourceTexture::Invalid,
                    ]
                };
                self.cs_clip_image.bind(&mut self.device, projection);
                self.draw_instanced_batch(items,
                                          vao,
                                          &textures);
            }
        }
    }

    fn update_deferred_resolves(&mut self, frame: &mut Frame) {
        // The first thing we do is run through any pending deferred
        // resolves, and use a callback to get the UV rect for this
        // custom item. Then we patch the resource_rects structure
        // here before it's uploaded to the GPU.
        if !frame.deferred_resolves.is_empty() {
            let handler = self.external_image_handler
                              .as_mut()
                              .expect("Found external image, but no handler set!");

            for deferred_resolve in &frame.deferred_resolves {
                GpuMarker::fire(self.device.gl(), "deferred resolve");
                let props = &deferred_resolve.image_properties;
                let ext_image = props.external_image
                                     .expect("BUG: Deferred resolves must be external images!");
                let image = handler.lock(ext_image.id, ext_image.channel_index);
                let texture_target = match ext_image.image_type {
                    ExternalImageType::Texture2DHandle => TextureTarget::Default,
                    ExternalImageType::TextureRectHandle => TextureTarget::Rect,
                    ExternalImageType::TextureExternalHandle => TextureTarget::External,
                    ExternalImageType::ExternalBuffer => {
                        panic!("{:?} is not a suitable image type in update_deferred_resolves().",
                            ext_image.image_type);
                    }
                };

                let texture_id = match image.source {
                    ExternalImageSource::NativeTexture(texture_id) => TextureId::new(texture_id, texture_target),
                    _ => panic!("No native texture found."),
                };

                self.external_images.insert((ext_image.id, ext_image.channel_index), texture_id);

                let update = GpuCacheUpdate::Copy {
                    block_index: 0,
                    block_count: 1,
                    address: deferred_resolve.address,
                };
                let blocks = [ [image.u0, image.v0, image.u1, image.v1].into() ];
                self.gpu_cache_texture.apply_patch(&update, &blocks);
            }
        }
    }

    fn unlock_external_images(&mut self) {
        if !self.external_images.is_empty() {
            let handler = self.external_image_handler
                              .as_mut()
                              .expect("Found external image, but no handler set!");

            for (ext_data, _) in self.external_images.drain() {
                handler.unlock(ext_data.0, ext_data.1);
            }
        }
    }

    fn start_frame(&mut self, frame: &mut Frame) {
        let _gm = self.gpu_profile.add_marker(GPU_TAG_SETUP_DATA);

        // Assign render targets to the passes.
        for pass in &mut frame.passes {
            debug_assert!(pass.color_texture_id.is_none());
            debug_assert!(pass.alpha_texture_id.is_none());

            if pass.needs_render_target_kind(RenderTargetKind::Color) {
                pass.color_texture_id = Some(self.color_render_targets
                                                 .pop()
                                                 .unwrap_or_else(|| {
                                                     self.device
                                                         .create_texture_ids(1, TextureTarget::Array)[0]
                                                  }));
            }

            if pass.needs_render_target_kind(RenderTargetKind::Alpha) {
                pass.alpha_texture_id = Some(self.alpha_render_targets
                                                 .pop()
                                                 .unwrap_or_else(|| {
                                                     self.device
                                                         .create_texture_ids(1, TextureTarget::Array)[0]
                                                  }));
            }
        }


        // Init textures and render targets to match this scene.
        for pass in &frame.passes {
            if let Some(texture_id) = pass.color_texture_id {
                let target_count = pass.required_target_count(RenderTargetKind::Color);
                self.device.init_texture(texture_id,
                                         frame.cache_size.width as u32,
                                         frame.cache_size.height as u32,
                                         ImageFormat::BGRA8,
                                         TextureFilter::Linear,
                                         RenderTargetMode::LayerRenderTarget(target_count as i32),
                                         None);
            }
            if let Some(texture_id) = pass.alpha_texture_id {
                let target_count = pass.required_target_count(RenderTargetKind::Alpha);
                self.device.init_texture(texture_id,
                                         frame.cache_size.width as u32,
                                         frame.cache_size.height as u32,
                                         ImageFormat::A8,
                                         TextureFilter::Nearest,
                                         RenderTargetMode::LayerRenderTarget(target_count as i32),
                                         None);
            }
        }

        // TODO(gw): This is a hack / workaround for #728.
        // We should find a better way to implement these updates rather
        // than wasting this extra memory, but for now it removes a large
        // number of driver stalls.
        self.gpu_data_textures[self.gdt_index].init_frame(&mut self.device, frame);
        self.gdt_index = (self.gdt_index + 1) % GPU_DATA_TEXTURE_POOL;
    }

    fn draw_tile_frame(&mut self,
                       frame: &mut Frame,
                       framebuffer_size: &DeviceUintSize) {
        let _gm = GpuMarker::new(self.device.rc_gl(), "tile frame draw");

        // Some tests use a restricted viewport smaller than the main screen size.
        // Ensure we clear the framebuffer in these tests.
        // TODO(gw): Find a better solution for this?
        let needs_clear = frame.window_size.width < framebuffer_size.width ||
                          frame.window_size.height < framebuffer_size.height;

        self.device.disable_depth_write();
        self.device.disable_stencil();
        self.device.set_blend(false);

        if frame.passes.is_empty() {
            self.device.clear_target(Some(self.clear_color.to_array()), Some(1.0));
        } else {
            self.start_frame(frame);

            let mut src_color_id = self.dummy_cache_texture_id;
            let mut src_alpha_id = self.dummy_cache_texture_id;

            for pass in &mut frame.passes {
                let size;
                let clear_color;
                let projection;

                if pass.is_framebuffer {
                    clear_color = if self.clear_framebuffer || needs_clear {
                        Some(frame.background_color.map_or(self.clear_color.to_array(), |color| {
                            color.to_array()
                        }))
                    } else {
                        None
                    };
                    size = framebuffer_size;
                    projection = Transform3D::ortho(0.0,
                                                 size.width as f32,
                                                 size.height as f32,
                                                 0.0,
                                                 ORTHO_NEAR_PLANE,
                                                 ORTHO_FAR_PLANE)
                } else {
                    size = &frame.cache_size;
                    clear_color = Some([0.0, 0.0, 0.0, 0.0]);
                    projection = Transform3D::ortho(0.0,
                                                 size.width as f32,
                                                 0.0,
                                                 size.height as f32,
                                                 ORTHO_NEAR_PLANE,
                                                 ORTHO_FAR_PLANE);
                }

                self.device.bind_texture(TextureSampler::CacheA8, src_alpha_id);
                self.device.bind_texture(TextureSampler::CacheRGBA8, src_color_id);

                for (target_index, target) in pass.alpha_targets.targets.iter().enumerate() {
                    self.draw_alpha_target((pass.alpha_texture_id.unwrap(), target_index as i32),
                                           target,
                                           *size,
                                           &projection);
                }

                for (target_index, target) in pass.color_targets.targets.iter().enumerate() {
                    let render_target = pass.color_texture_id.map(|texture_id| {
                        (texture_id, target_index as i32)
                    });
                    self.draw_color_target(render_target,
                                           target,
                                           *size,
                                           src_color_id,
                                           clear_color,
                                           &frame.render_task_data,
                                           &projection);

                }

                src_color_id = pass.color_texture_id.unwrap_or(self.dummy_cache_texture_id);
                src_alpha_id = pass.alpha_texture_id.unwrap_or(self.dummy_cache_texture_id);

                // Return the texture IDs to the pool for next frame.
                if let Some(texture_id) = pass.color_texture_id.take() {
                    self.color_render_targets.push(texture_id);
                }
                if let Some(texture_id) = pass.alpha_texture_id.take() {
                    self.alpha_render_targets.push(texture_id);
                }
            }

            self.color_render_targets.reverse();
            self.alpha_render_targets.reverse();
            self.draw_render_target_debug(framebuffer_size);
            self.draw_texture_cache_debug(framebuffer_size);
        }

        self.unlock_external_images();
    }

    pub fn debug_renderer<'a>(&'a mut self) -> &'a mut DebugRenderer {
        &mut self.debug
    }

    pub fn get_debug_flags(&self) -> DebugFlags {
        self.debug_flags
    }

    pub fn set_debug_flags(&mut self, flags: DebugFlags) {
        self.debug_flags = flags;
    }

    pub fn save_cpu_profile(&self, filename: &str) {
        write_profile(filename);
    }

    fn draw_render_target_debug(&mut self,
                                framebuffer_size: &DeviceUintSize) {
        if !self.debug_flags.contains(RENDER_TARGET_DBG) {
            return;
        }

        let mut spacing = 16;
        let mut size = 512;
        let fb_width = framebuffer_size.width as i32;
        let num_textures = self.color_render_targets.iter().chain(self.alpha_render_targets.iter()).count() as i32;

        if num_textures * (size + spacing) > fb_width {
            let factor = fb_width as f32 / (num_textures * (size + spacing)) as f32;
            size = (size as f32 * factor) as i32;
            spacing = (spacing as f32 * factor) as i32;
        }

        for (i, texture_id) in self.color_render_targets.iter().chain(self.alpha_render_targets.iter()).enumerate() {
            let layer_count = self.device.get_render_target_layer_count(*texture_id);
            for layer_index in 0..layer_count {
                let x = fb_width - (spacing + size) * (i as i32 + 1);
                let y = spacing;

                let dest_rect = rect(x, y, size, size);
                self.device.blit_render_target(
                    Some((*texture_id, layer_index as i32)),
                    None,
                    dest_rect
                );
            }
        }
    }

    fn draw_texture_cache_debug(&mut self, framebuffer_size: &DeviceUintSize) {
        if !self.debug_flags.contains(TEXTURE_CACHE_DBG) {
            return;
        }

        let mut spacing = 16;
        let mut size = 512;
        let fb_width = framebuffer_size.width as i32;
        let num_textures = self.cache_texture_id_map.len() as i32;

        if num_textures * (size + spacing) > fb_width {
            let factor = fb_width as f32 / (num_textures * (size + spacing)) as f32;
            size = (size as f32 * factor) as i32;
            spacing = (spacing as f32 * factor) as i32;
        }

        for (i, texture_id) in self.cache_texture_id_map.iter().enumerate() {
            let x = fb_width - (spacing + size) * (i as i32 + 1);
            let y = spacing + if self.debug_flags.contains(RENDER_TARGET_DBG) { 528 } else { 0 };

            // If we have more targets than fit on one row in screen, just early exit.
            if x > fb_width {
                return;
            }

            let dest_rect = rect(x, y, size, size);
            self.device.blit_render_target(Some((*texture_id, 0)), None, dest_rect);
        }
    }

    pub fn read_pixels_rgba8(&self, rect: DeviceUintRect) -> Vec<u8> {
        let mut pixels = vec![0u8; (4 * rect.size.width * rect.size.height) as usize];
        self.read_pixels_into(rect, ReadPixelsFormat::Rgba8, &mut pixels);
        pixels
    }

    pub fn read_pixels_into(&self,
                            rect: DeviceUintRect,
                            format: ReadPixelsFormat,
                            output: &mut [u8]) {
        let (gl_format, gl_type, size) = match format {
            ReadPixelsFormat::Rgba8 => (gl::RGBA, gl::UNSIGNED_BYTE, 4),
            ReadPixelsFormat::Bgra8 => (get_gl_format_bgra(self.device.gl()), gl::UNSIGNED_BYTE, 4),
        };
        assert_eq!(output.len(), (size * rect.size.width * rect.size.height) as usize);
        self.device.gl().flush();
        self.device.gl().read_pixels_into_buffer(rect.origin.x as gl::GLint,
                                                 rect.origin.y as gl::GLint,
                                                 rect.size.width as gl::GLsizei,
                                                 rect.size.height as gl::GLsizei,
                                                 gl_format,
                                                 gl_type,
                                                 output);
    }

    // De-initialize the Renderer safely, assuming the GL is still alive and active.
    pub fn deinit(mut self) {
        //Note: this is a fake frame, only needed because texture deletion is require to happen inside a frame
        self.device.begin_frame(1.0);
        self.device.deinit_texture(self.dummy_cache_texture_id);
        self.debug.deinit(&mut self.device);
        self.cs_box_shadow.deinit(&mut self.device);
        self.cs_text_run.deinit(&mut self.device);
        self.cs_line.deinit(&mut self.device);
        self.cs_blur.deinit(&mut self.device);
        self.cs_clip_rectangle.deinit(&mut self.device);
        self.cs_clip_image.deinit(&mut self.device);
        self.cs_clip_border.deinit(&mut self.device);
        self.ps_rectangle.deinit(&mut self.device);
        self.ps_rectangle_clip.deinit(&mut self.device);
        self.ps_text_run.deinit(&mut self.device);
        self.ps_text_run_subpixel.deinit(&mut self.device);
        for shader in &mut self.ps_image {
            if let &mut Some(ref mut shader) = shader {
                shader.deinit(&mut self.device);
            }
        }
        for shader in &mut self.ps_yuv_image {
            if let &mut Some(ref mut shader) = shader {
                shader.deinit(&mut self.device);
            }
        }
        self.ps_border_corner.deinit(&mut self.device);
        self.ps_border_edge.deinit(&mut self.device);
        self.ps_gradient.deinit(&mut self.device);
        self.ps_angle_gradient.deinit(&mut self.device);
        self.ps_radial_gradient.deinit(&mut self.device);
        self.ps_box_shadow.deinit(&mut self.device);
        self.ps_cache_image.deinit(&mut self.device);
        self.ps_line.deinit(&mut self.device);
        self.ps_blend.deinit(&mut self.device);
        self.ps_hw_composite.deinit(&mut self.device);
        self.ps_split_composite.deinit(&mut self.device);
        self.ps_composite.deinit(&mut self.device);
        self.device.end_frame();
    }
}

pub enum ExternalImageSource<'a> {
    RawData(&'a [u8]),      // raw buffers.
    NativeTexture(u32),     // Is a gl::GLuint texture handle
}

/// The data that an external client should provide about
/// an external image. The timestamp is used to test if
/// the renderer should upload new texture data this
/// frame. For instance, if providing video frames, the
/// application could call wr.render() whenever a new
/// video frame is ready. If the callback increments
/// the returned timestamp for a given image, the renderer
/// will know to re-upload the image data to the GPU.
/// Note that the UV coords are supplied in texel-space!
pub struct ExternalImage<'a> {
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
    pub source: ExternalImageSource<'a>,
}

/// The interfaces that an application can implement to support providing
/// external image buffers.
/// When the the application passes an external image to WR, it should kepp that
/// external image life time. People could check the epoch id in RenderNotifier
/// at the client side to make sure that the external image is not used by WR.
/// Then, do the clean up for that external image.
pub trait ExternalImageHandler {
    /// Lock the external image. Then, WR could start to read the image content.
    /// The WR client should not change the image content until the unlock()
    /// call.
    fn lock(&mut self, key: ExternalImageId, channel_index: u8) -> ExternalImage;
    /// Unlock the external image. The WR should not read the image content
    /// after this call.
    fn unlock(&mut self, key: ExternalImageId, channel_index: u8);
}

pub struct RendererOptions {
    pub device_pixel_ratio: f32,
    pub resource_override_path: Option<PathBuf>,
    pub enable_aa: bool,
    pub enable_dithering: bool,
    pub max_recorded_profiles: usize,
    pub debug: bool,
    pub enable_scrollbars: bool,
    pub precache_shaders: bool,
    pub renderer_kind: RendererKind,
    pub enable_subpixel_aa: bool,
    pub clear_framebuffer: bool,
    pub clear_color: ColorF,
    pub enable_clear_scissor: bool,
    pub enable_batcher: bool,
    pub max_texture_size: Option<u32>,
    pub cache_expiry_frames: u32,
    pub workers: Option<Arc<ThreadPool>>,
    pub blob_image_renderer: Option<Box<BlobImageRenderer>>,
    pub recorder: Option<Box<ApiRecordingReceiver>>,
    pub enable_render_on_scroll: bool,
    pub debug_flags: DebugFlags,
}

impl Default for RendererOptions {
    fn default() -> RendererOptions {
        RendererOptions {
            device_pixel_ratio: 1.0,
            resource_override_path: None,
            enable_aa: true,
            enable_dithering: true,
            debug_flags: DebugFlags::empty(),
            max_recorded_profiles: 0,
            debug: false,
            enable_scrollbars: false,
            precache_shaders: false,
            renderer_kind: RendererKind::Native,
            enable_subpixel_aa: false,
            clear_framebuffer: true,
            clear_color: ColorF::new(1.0, 1.0, 1.0, 1.0),
            enable_clear_scissor: true,
            enable_batcher: true,
            max_texture_size: None,
            cache_expiry_frames: 600, // roughly, 10 seconds
            workers: None,
            blob_image_renderer: None,
            recorder: None,
            enable_render_on_scroll: true,
        }
    }
}
