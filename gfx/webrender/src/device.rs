/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use euclid::Transform3D;
use gleam::gl;
use internal_types::{RenderTargetMode, TextureSampler, DEFAULT_TEXTURE, FastHashMap};
//use notify::{self, Watcher};
use super::shader_source;
use std::fs::File;
use std::io::Read;
use std::iter::repeat;
use std::mem;
use std::ops::Add;
use std::path::PathBuf;
use std::ptr;
use std::rc::Rc;
//use std::sync::mpsc::{channel, Sender};
use std::thread;
use api::{ColorF, ImageFormat};
use api::{DeviceIntPoint, DeviceIntRect, DeviceIntSize, DeviceUintSize};

#[derive(Debug, Copy, Clone, PartialEq, Ord, Eq, PartialOrd)]
pub struct FrameId(usize);

impl FrameId {
    pub fn new(value: usize) -> FrameId {
        FrameId(value)
    }
}

impl Add<usize> for FrameId {
    type Output = FrameId;

    fn add(self, other: usize) -> FrameId {
        FrameId(self.0 + other)
    }
}

#[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
const GL_FORMAT_A: gl::GLuint = gl::RED;

#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
const GL_FORMAT_A: gl::GLuint = gl::ALPHA;

const GL_FORMAT_BGRA_GL: gl::GLuint = gl::BGRA;

const GL_FORMAT_BGRA_GLES: gl::GLuint = gl::BGRA_EXT;

const SHADER_VERSION_GL: &str = "#version 150\n";

const SHADER_VERSION_GLES: &str = "#version 300 es\n";

static SHADER_PREAMBLE: &str = "shared";

#[repr(u32)]
pub enum DepthFunction {
    Less = gl::LESS,
    LessEqual = gl::LEQUAL,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum TextureTarget {
    Default,
    Array,
    Rect,
    External,
}

impl TextureTarget {
    pub fn to_gl_target(&self) -> gl::GLuint {
        match *self {
            TextureTarget::Default => gl::TEXTURE_2D,
            TextureTarget::Array => gl::TEXTURE_2D_ARRAY,
            TextureTarget::Rect => gl::TEXTURE_RECTANGLE,
            TextureTarget::External => gl::TEXTURE_EXTERNAL_OES,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum TextureFilter {
    Nearest,
    Linear,
}

#[derive(Debug)]
pub enum VertexAttributeKind {
    F32,
    U8Norm,
    I32,
}

#[derive(Debug)]
pub struct VertexAttribute {
    pub name: &'static str,
    pub count: u32,
    pub kind: VertexAttributeKind,
}

#[derive(Debug)]
pub struct VertexDescriptor {
    pub vertex_attributes: &'static [VertexAttribute],
    pub instance_attributes: &'static [VertexAttribute],
}

enum FBOTarget {
    Read,
    Draw,
}

pub fn get_gl_format_bgra(gl: &gl::Gl) -> gl::GLuint {
    match gl.get_type() {
        gl::GlType::Gl => {
            GL_FORMAT_BGRA_GL
        }
        gl::GlType::Gles => {
            GL_FORMAT_BGRA_GLES
        }
    }
}

fn get_shader_version(gl: &gl::Gl) -> &'static str {
    match gl.get_type() {
        gl::GlType::Gl => {
            SHADER_VERSION_GL
        }
        gl::GlType::Gles => {
            SHADER_VERSION_GLES
        }
    }
}

fn get_optional_shader_source(shader_name: &str, base_path: &Option<PathBuf>) -> Option<String> {
    if let Some(ref base) = *base_path {
        let shader_path = base.join(&format!("{}.glsl", shader_name));
        if shader_path.exists() {
            let mut source = String::new();
            File::open(&shader_path).unwrap().read_to_string(&mut source).unwrap();
            return Some(source);
        }
    }

    shader_source::SHADERS.get(shader_name).and_then(|s| Some((*s).to_owned()))
}

fn get_shader_source(shader_name: &str, base_path: &Option<PathBuf>) -> String {
    get_optional_shader_source(shader_name, base_path)
        .expect(&format!("Couldn't get required shader: {}", shader_name))
}

pub trait FileWatcherHandler : Send {
    fn file_changed(&self, path: PathBuf);
}

impl VertexAttributeKind {
    fn size_in_bytes(&self) -> u32 {
        match *self {
            VertexAttributeKind::F32 => 4,
            VertexAttributeKind::U8Norm => 1,
            VertexAttributeKind::I32 => 4,
        }
    }
}

impl VertexAttribute {
    fn size_in_bytes(&self) -> u32 {
        self.count * self.kind.size_in_bytes()
    }

    fn bind_to_vao(&self,
                   attr_index: gl::GLuint,
                   divisor: gl::GLuint,
                   stride: gl::GLint,
                   offset: gl::GLuint,
                   gl: &gl::Gl) {
        gl.enable_vertex_attrib_array(attr_index);
        gl.vertex_attrib_divisor(attr_index, divisor);

        match self.kind {
            VertexAttributeKind::F32 => {
                gl.vertex_attrib_pointer(attr_index,
                                         self.count as gl::GLint,
                                         gl::FLOAT,
                                         false,
                                         stride,
                                         offset);
            }
            VertexAttributeKind::U8Norm => {
                gl.vertex_attrib_pointer(attr_index,
                                         self.count as gl::GLint,
                                         gl::UNSIGNED_BYTE,
                                         true,
                                         stride,
                                         offset);
            }
            VertexAttributeKind::I32 => {
                gl.vertex_attrib_i_pointer(attr_index,
                                           self.count as gl::GLint,
                                           gl::INT,
                                           stride,
                                           offset);
            }
        }
    }
}

impl VertexDescriptor {
    fn bind(&self,
            gl: &gl::Gl,
            main: VBOId,
            instance: VBOId) {
        main.bind(gl);

        let vertex_stride: u32 = self.vertex_attributes
                                    .iter()
                                    .map(|attr| attr.size_in_bytes()).sum();
        let mut vertex_offset = 0;

        for (i, attr) in self.vertex_attributes.iter().enumerate() {
            let attr_index = i as gl::GLuint;
            attr.bind_to_vao(attr_index,
                             0,
                             vertex_stride as gl::GLint,
                             vertex_offset,
                             gl);
            vertex_offset += attr.size_in_bytes();
        }

        if !self.instance_attributes.is_empty() {
            instance.bind(gl);
            let instance_stride: u32 = self.instance_attributes
                                           .iter()
                                           .map(|attr| attr.size_in_bytes()).sum();
            let mut instance_offset = 0;

            let base_attr = self.vertex_attributes.len() as u32;

            for (i, attr) in self.instance_attributes.iter().enumerate() {
                let attr_index = base_attr + i as u32;
                attr.bind_to_vao(attr_index,
                                 1,
                                 instance_stride as gl::GLint,
                                 instance_offset,
                                 gl);
                instance_offset += attr.size_in_bytes();
            }
        }
    }
}

impl TextureId {
    pub fn bind(&self, gl: &gl::Gl) {
        gl.bind_texture(self.target, self.name);
    }

    pub fn new(name: gl::GLuint, texture_target: TextureTarget) -> TextureId {
        TextureId {
            name,
            target: texture_target.to_gl_target(),
        }
    }

    pub fn invalid() -> TextureId {
        TextureId {
            name: 0,
            target: gl::TEXTURE_2D,
        }
    }

    pub fn is_valid(&self) -> bool { *self != TextureId::invalid() }
}

impl VBOId {
    fn bind(&self, gl: &gl::Gl) {
        gl.bind_buffer(gl::ARRAY_BUFFER, self.0);
    }
}

impl IBOId {
    fn bind(&self, gl: &gl::Gl) {
        gl.bind_buffer(gl::ELEMENT_ARRAY_BUFFER, self.0);
    }
}

impl FBOId {
    fn bind(&self, gl: &gl::Gl, target: FBOTarget) {
        let target = match target {
            FBOTarget::Read => gl::READ_FRAMEBUFFER,
            FBOTarget::Draw => gl::DRAW_FRAMEBUFFER,
        };
        gl.bind_framebuffer(target, self.0);
    }
}

struct Texture {
    gl: Rc<gl::Gl>,
    id: gl::GLuint,
    format: ImageFormat,
    width: u32,
    height: u32,

    filter: TextureFilter,
    mode: RenderTargetMode,
    fbo_ids: Vec<FBOId>,
    depth_rb: Option<RBOId>,
}

impl Drop for Texture {
    fn drop(&mut self) {
        if !self.fbo_ids.is_empty() {
            let fbo_ids: Vec<_> = self.fbo_ids.iter().map(|&FBOId(fbo_id)| fbo_id).collect();
            self.gl.delete_framebuffers(&fbo_ids[..]);
        }
        self.gl.delete_textures(&[self.id]);
    }
}

pub struct Program {
    id: gl::GLuint,
    u_transform: gl::GLint,
    u_device_pixel_ratio: gl::GLint,
    name: String,
    vs_source: String,
    fs_source: String,
    prefix: Option<String>,
    vs_id: Option<gl::GLuint>,
    fs_id: Option<gl::GLuint>,
}

impl Program {
    fn attach_and_bind_shaders(&mut self,
                               vs_id: gl::GLuint,
                               fs_id: gl::GLuint,
                               descriptor: &VertexDescriptor,
                               gl: &gl::Gl) -> Result<(), ShaderError> {
        gl.attach_shader(self.id, vs_id);
        gl.attach_shader(self.id, fs_id);

        for (i, attr) in descriptor.vertex_attributes
                                   .iter()
                                   .chain(descriptor.instance_attributes.iter())
                                   .enumerate() {
            gl.bind_attrib_location(self.id,
                                    i as gl::GLuint,
                                    attr.name);
        }

        gl.link_program(self.id);
        if gl.get_program_iv(self.id, gl::LINK_STATUS) == (0 as gl::GLint) {
            let error_log = gl.get_program_info_log(self.id);
            println!("Failed to link shader program: {:?}\n{}", self.name, error_log);
            gl.detach_shader(self.id, vs_id);
            gl.detach_shader(self.id, fs_id);
            return Err(ShaderError::Link(self.name.clone(), error_log));
        }

        Ok(())
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        debug_assert!(thread::panicking() || self.id == 0);
    }
}

struct VAO {
    gl: Rc<gl::Gl>,
    id: gl::GLuint,
    ibo_id: IBOId,
    main_vbo_id: VBOId,
    instance_vbo_id: VBOId,
    instance_stride: gl::GLint,
    owns_indices: bool,
    owns_vertices: bool,
    owns_instances: bool,
}

impl Drop for VAO {
    fn drop(&mut self) {
        self.gl.delete_vertex_arrays(&[self.id]);

        if self.owns_indices {
            // todo(gw): maybe make these their own type with hashmap?
            self.gl.delete_buffers(&[self.ibo_id.0]);
        }
        if self.owns_vertices {
            self.gl.delete_buffers(&[self.main_vbo_id.0]);
        }
        if self.owns_instances {
            self.gl.delete_buffers(&[self.instance_vbo_id.0])
        }
    }
}

#[derive(PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Copy, Clone)]
pub struct TextureId {
    name: gl::GLuint,
    target: gl::GLuint,
}

#[derive(PartialEq, Eq, Hash, Debug, Copy, Clone)]
pub struct VAOId(gl::GLuint);

#[derive(PartialEq, Eq, Hash, Debug, Copy, Clone)]
pub struct FBOId(gl::GLuint);

#[derive(PartialEq, Eq, Hash, Debug, Copy, Clone)]
pub struct RBOId(gl::GLuint);

#[derive(PartialEq, Eq, Hash, Debug, Copy, Clone)]
pub struct VBOId(gl::GLuint);

#[derive(PartialEq, Eq, Hash, Debug, Copy, Clone)]
struct IBOId(gl::GLuint);

#[derive(PartialEq, Eq, Hash, Debug, Copy, Clone)]
pub struct PBOId(gl::GLuint);

const MAX_EVENTS_PER_FRAME: usize = 256;
const MAX_PROFILE_FRAMES: usize = 4;

pub trait NamedTag {
    fn get_label(&self) -> &str;
}

#[derive(Debug, Clone)]
pub struct GpuSample<T> {
    pub tag: T,
    pub time_ns: u64,
}

pub struct GpuFrameProfile<T> {
    gl: Rc<gl::Gl>,
    queries: Vec<gl::GLuint>,
    samples: Vec<GpuSample<T>>,
    next_query: usize,
    pending_query: gl::GLuint,
    frame_id: FrameId,
    inside_frame: bool,
}

impl<T> GpuFrameProfile<T> {
    fn new(gl: Rc<gl::Gl>) -> Self {
        match gl.get_type() {
            gl::GlType::Gl => {
                let queries = gl.gen_queries(MAX_EVENTS_PER_FRAME as gl::GLint);
                GpuFrameProfile {
                    gl,
                    queries,
                    samples: Vec::new(),
                    next_query: 0,
                    pending_query: 0,
                    frame_id: FrameId(0),
                    inside_frame: false,
                }
            }
            gl::GlType::Gles => {
                GpuFrameProfile {
                    gl,
                    queries: Vec::new(),
                    samples: Vec::new(),
                    next_query: 0,
                    pending_query: 0,
                    frame_id: FrameId(0),
                    inside_frame: false,
                }
            }
        }
    }

    fn begin_frame(&mut self, frame_id: FrameId) {
        self.frame_id = frame_id;
        self.next_query = 0;
        self.pending_query = 0;
        self.samples.clear();
        self.inside_frame = true;
    }

    fn end_frame(&mut self) {
        self.inside_frame = false;
        match self.gl.get_type() {
            gl::GlType::Gl => {
                if self.pending_query != 0 {
                    self.gl.end_query(gl::TIME_ELAPSED);
                }
            }
            gl::GlType::Gles => {},
        }
    }

    fn add_marker(&mut self, tag: T) -> GpuMarker
    where T: NamedTag {
        debug_assert!(self.inside_frame);
        match self.gl.get_type() {
            gl::GlType::Gl => {
                self.add_marker_gl(tag)
            }
            gl::GlType::Gles => {
                self.add_marker_gles(tag)
            }
        }
    }

    fn add_marker_gl(&mut self, tag: T) -> GpuMarker
    where T: NamedTag {
        if self.pending_query != 0 {
            self.gl.end_query(gl::TIME_ELAPSED);
        }

        let marker = GpuMarker::new(&self.gl, tag.get_label());

        if self.next_query < MAX_EVENTS_PER_FRAME {
            self.pending_query = self.queries[self.next_query];
            self.gl.begin_query(gl::TIME_ELAPSED, self.pending_query);
            self.samples.push(GpuSample {
                tag,
                time_ns: 0,
            });
        } else {
            self.pending_query = 0;
        }

        self.next_query += 1;
        marker
    }

    fn add_marker_gles(&mut self, tag: T) -> GpuMarker
    where T: NamedTag {
        let marker = GpuMarker::new(&self.gl, tag.get_label());
        self.samples.push(GpuSample {
            tag,
            time_ns: 0,
        });
        marker
    }

    fn is_valid(&self) -> bool {
        self.next_query > 0 && self.next_query <= MAX_EVENTS_PER_FRAME
    }

    fn build_samples(&mut self) -> Vec<GpuSample<T>> {
        debug_assert!(!self.inside_frame);
        match self.gl.get_type() {
            gl::GlType::Gl => {
                self.build_samples_gl()
            }
            gl::GlType::Gles => {
                self.build_samples_gles()
            }
        }
    }

    fn build_samples_gl(&mut self) -> Vec<GpuSample<T>> {
        for (index, sample) in self.samples.iter_mut().enumerate() {
            sample.time_ns = self.gl.get_query_object_ui64v(self.queries[index], gl::QUERY_RESULT)
        }

        mem::replace(&mut self.samples, Vec::new())
    }

    fn build_samples_gles(&mut self) -> Vec<GpuSample<T>> {
        mem::replace(&mut self.samples, Vec::new())
    }
}

impl<T> Drop for GpuFrameProfile<T> {
    fn drop(&mut self) {
        match self.gl.get_type() {
            gl::GlType::Gl =>  {
                self.gl.delete_queries(&self.queries);
            }
            gl::GlType::Gles => {},
        }
    }
}

pub struct GpuProfiler<T> {
    frames: [GpuFrameProfile<T>; MAX_PROFILE_FRAMES],
    next_frame: usize,
}

impl<T> GpuProfiler<T> {
    pub fn new(gl: &Rc<gl::Gl>) -> GpuProfiler<T> {
        GpuProfiler {
            next_frame: 0,
            frames: [
                      GpuFrameProfile::new(Rc::clone(gl)),
                      GpuFrameProfile::new(Rc::clone(gl)),
                      GpuFrameProfile::new(Rc::clone(gl)),
                      GpuFrameProfile::new(Rc::clone(gl)),
                    ],
        }
    }

    pub fn build_samples(&mut self) -> Option<(FrameId, Vec<GpuSample<T>>)> {
        let frame = &mut self.frames[self.next_frame];
        if frame.is_valid() {
            Some((frame.frame_id, frame.build_samples()))
        } else {
            None
        }
    }

    pub fn begin_frame(&mut self, frame_id: FrameId) {
        let frame = &mut self.frames[self.next_frame];
        frame.begin_frame(frame_id);
    }

    pub fn end_frame(&mut self) {
        let frame = &mut self.frames[self.next_frame];
        frame.end_frame();
        self.next_frame = (self.next_frame + 1) % MAX_PROFILE_FRAMES;
    }

    pub fn add_marker(&mut self, tag: T) -> GpuMarker
    where T: NamedTag {
        self.frames[self.next_frame].add_marker(tag)
    }
}

#[must_use]
pub struct GpuMarker{
    gl: Rc<gl::Gl>,
}

impl GpuMarker {
    pub fn new(gl: &Rc<gl::Gl>, message: &str) -> GpuMarker {
        match gl.get_type() {
            gl::GlType::Gl =>  {
                gl.push_group_marker_ext(message);
                GpuMarker{
                    gl: Rc::clone(gl),
                }
            }
            gl::GlType::Gles => {
                GpuMarker{
                    gl: Rc::clone(gl),
                }
            }
        }
    }

    pub fn fire(gl: &gl::Gl, message: &str) {
        match gl.get_type() {
            gl::GlType::Gl =>  {
                gl.insert_event_marker_ext(message);
            }
            gl::GlType::Gles => {},
        }
    }
}

#[cfg(not(any(target_arch="arm", target_arch="aarch64")))]
impl Drop for GpuMarker {
    fn drop(&mut self) {
        match self.gl.get_type() {
            gl::GlType::Gl =>  {
                self.gl.pop_group_marker_ext();
            }
            gl::GlType::Gles => {},
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub enum VertexUsageHint {
    Static,
    Dynamic,
    Stream,
}

impl VertexUsageHint {
    fn to_gl(&self) -> gl::GLuint {
        match *self {
            VertexUsageHint::Static => gl::STATIC_DRAW,
            VertexUsageHint::Dynamic => gl::DYNAMIC_DRAW,
            VertexUsageHint::Stream => gl::STREAM_DRAW,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct UniformLocation(gl::GLint);

impl UniformLocation {
    pub fn invalid() -> UniformLocation {
        UniformLocation(-1)
    }
}

// TODO(gw): Fix up notify cargo deps and re-enable this!
/*
enum FileWatcherCmd {
    AddWatch(PathBuf),
    Exit,
}

struct FileWatcherThread {
    api_tx: Sender<FileWatcherCmd>,
}

impl FileWatcherThread {
    fn new(handler: Box<FileWatcherHandler>) -> FileWatcherThread {
        let (api_tx, api_rx) = channel();

        thread::spawn(move || {

            let (watch_tx, watch_rx) = channel();

            enum Request {
                Watcher(notify::Event),
                Command(FileWatcherCmd),
            }

            let mut file_watcher: notify::RecommendedWatcher = notify::Watcher::new(watch_tx).unwrap();

            loop {
                let request = {
                    let receiver_from_api = &api_rx;
                    let receiver_from_watcher = &watch_rx;
                    select! {
                        msg = receiver_from_api.recv() => Request::Command(msg.unwrap()),
                        msg = receiver_from_watcher.recv() => Request::Watcher(msg.unwrap())
                    }
                };

                match request {
                    Request::Watcher(event) => {
                        handler.file_changed(event.path.unwrap());
                    }
                    Request::Command(cmd) => {
                        match cmd {
                            FileWatcherCmd::AddWatch(path) => {
                                file_watcher.watch(path).ok();
                            }
                            FileWatcherCmd::Exit => {
                                break;
                            }
                        }
                    }
                }
            }
        });

        FileWatcherThread {
            api_tx,
        }
    }

    fn exit(&self) {
        self.api_tx.send(FileWatcherCmd::Exit).ok();
    }

    fn add_watch(&self, path: PathBuf) {
        self.api_tx.send(FileWatcherCmd::AddWatch(path)).ok();
    }
}
*/

pub struct Capabilities {
    pub supports_multisampling: bool,
}

#[derive(Clone, Debug)]
pub enum ShaderError {
    Compilation(String, String), // name, error mssage
    Link(String, String), // name, error message
}

pub struct Device {
    gl: Rc<gl::Gl>,
    // device state
    bound_textures: [TextureId; 16],
    bound_program: gl::GLuint,
    bound_vao: VAOId,
    bound_pbo: PBOId,
    bound_read_fbo: FBOId,
    bound_draw_fbo: FBOId,
    default_read_fbo: gl::GLuint,
    default_draw_fbo: gl::GLuint,
    device_pixel_ratio: f32,

    // HW or API capabilties
    capabilities: Capabilities,

    // debug
    inside_frame: bool,

    // resources
    resource_override_path: Option<PathBuf>,
    textures: FastHashMap<TextureId, Texture>,
    vaos: FastHashMap<VAOId, VAO>,

    // misc.
    shader_preamble: String,
    //file_watcher: FileWatcherThread,

    // Used on android only
    #[allow(dead_code)]
    next_vao_id: gl::GLuint,

    max_texture_size: u32,

    // Frame counter. This is used to map between CPU
    // frames and GPU frames.
    frame_id: FrameId,
}

impl Device {
    pub fn new(gl: Rc<gl::Gl>,
               resource_override_path: Option<PathBuf>,
               _file_changed_handler: Box<FileWatcherHandler>) -> Device {
        //let file_watcher = FileWatcherThread::new(file_changed_handler);

        let shader_preamble = get_shader_source(SHADER_PREAMBLE, &resource_override_path);
        //file_watcher.add_watch(resource_path);

        let max_texture_size = gl.get_integer_v(gl::MAX_TEXTURE_SIZE) as u32;

        Device {
            gl,
            resource_override_path,
            // This is initialized to 1 by default, but it is set
            // every frame by the call to begin_frame().
            device_pixel_ratio: 1.0,
            inside_frame: false,

            capabilities: Capabilities {
                supports_multisampling: false, //TODO
            },

            bound_textures: [ TextureId::invalid(); 16 ],
            bound_program: 0,
            bound_vao: VAOId(0),
            bound_pbo: PBOId(0),
            bound_read_fbo: FBOId(0),
            bound_draw_fbo: FBOId(0),
            default_read_fbo: 0,
            default_draw_fbo: 0,

            textures: FastHashMap::default(),
            vaos: FastHashMap::default(),

            shader_preamble,

            next_vao_id: 1,
            //file_watcher: file_watcher,

            max_texture_size,
            frame_id: FrameId(0),
        }
    }

    pub fn gl(&self) -> &gl::Gl {
        &*self.gl
    }

    pub fn rc_gl(&self) -> &Rc<gl::Gl> {
        &self.gl
    }

    pub fn max_texture_size(&self) -> u32 {
        self.max_texture_size
    }

    pub fn get_capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    pub fn compile_shader(gl: &gl::Gl,
                          name: &str,
                          source_str: &str,
                          shader_type: gl::GLenum,
                          shader_preamble: &[String])
                          -> Result<gl::GLuint, ShaderError> {
        debug!("compile {:?}", name);

        let mut s = String::new();
        s.push_str(get_shader_version(gl));
        for prefix in shader_preamble {
            s.push_str(prefix);
        }
        s.push_str(source_str);

        let id = gl.create_shader(shader_type);
        let mut source = Vec::new();
        source.extend_from_slice(s.as_bytes());
        gl.shader_source(id, &[&source[..]]);
        gl.compile_shader(id);
        let log = gl.get_shader_info_log(id);
        if gl.get_shader_iv(id, gl::COMPILE_STATUS) == (0 as gl::GLint) {
            println!("Failed to compile shader: {:?}\n{}", name, log);
            Err(ShaderError::Compilation(name.to_string(), log))
        } else {
            if !log.is_empty() {
                println!("Warnings detected on shader: {:?}\n{}", name, log);
            }
            Ok(id)
        }
    }

    pub fn begin_frame(&mut self, device_pixel_ratio: f32) -> FrameId {
        debug_assert!(!self.inside_frame);
        self.inside_frame = true;
        self.device_pixel_ratio = device_pixel_ratio;

        // Retrive the currently set FBO.
        let default_read_fbo = self.gl.get_integer_v(gl::READ_FRAMEBUFFER_BINDING);
        self.default_read_fbo = default_read_fbo as gl::GLuint;
        let default_draw_fbo = self.gl.get_integer_v(gl::DRAW_FRAMEBUFFER_BINDING);
        self.default_draw_fbo = default_draw_fbo as gl::GLuint;

        // Texture state
        for i in 0..self.bound_textures.len() {
            self.bound_textures[i] = TextureId::invalid();
            self.gl.active_texture(gl::TEXTURE0 + i as gl::GLuint);
            self.gl.bind_texture(gl::TEXTURE_2D, 0);
        }

        // Shader state
        self.bound_program = 0;
        self.gl.use_program(0);

        // Vertex state
        self.bound_vao = VAOId(0);
        self.clear_vertex_array();

        // FBO state
        self.bound_read_fbo = FBOId(self.default_read_fbo);
        self.bound_draw_fbo = FBOId(self.default_draw_fbo);

        // Pixel op state
        self.gl.pixel_store_i(gl::UNPACK_ALIGNMENT, 1);
        self.bound_pbo = PBOId(0);
        self.gl.bind_buffer(gl::PIXEL_UNPACK_BUFFER, 0);

        // Default is sampler 0, always
        self.gl.active_texture(gl::TEXTURE0);

        self.frame_id
    }

    pub fn bind_texture(&mut self,
                        sampler: TextureSampler,
                        texture_id: TextureId) {
        debug_assert!(self.inside_frame);

        let sampler_index = sampler as usize;
        if self.bound_textures[sampler_index] != texture_id {
            self.bound_textures[sampler_index] = texture_id;
            self.gl.active_texture(gl::TEXTURE0 + sampler_index as gl::GLuint);
            texture_id.bind(self.gl());
            self.gl.active_texture(gl::TEXTURE0);
        }
    }

    pub fn bind_read_target(&mut self, texture_id: Option<(TextureId, i32)>) {
        debug_assert!(self.inside_frame);

        let fbo_id = texture_id.map_or(FBOId(self.default_read_fbo), |texture_id| {
            self.textures.get(&texture_id.0).unwrap().fbo_ids[texture_id.1 as usize]
        });

        if self.bound_read_fbo != fbo_id {
            self.bound_read_fbo = fbo_id;
            fbo_id.bind(self.gl(), FBOTarget::Read);
        }
    }

    pub fn bind_draw_target(&mut self,
                            texture_id: Option<(TextureId, i32)>,
                            dimensions: Option<DeviceUintSize>) {
        debug_assert!(self.inside_frame);

        let fbo_id = texture_id.map_or(FBOId(self.default_draw_fbo), |texture_id| {
            self.textures.get(&texture_id.0).unwrap().fbo_ids[texture_id.1 as usize]
        });

        if self.bound_draw_fbo != fbo_id {
            self.bound_draw_fbo = fbo_id;
            fbo_id.bind(self.gl(), FBOTarget::Draw);
        }

        if let Some(dimensions) = dimensions {
            self.gl.viewport(0, 0, dimensions.width as gl::GLint, dimensions.height as gl::GLint);
        }
    }

    pub fn bind_program(&mut self, program: &Program) {
        debug_assert!(self.inside_frame);

        if self.bound_program != program.id {
            self.gl.use_program(program.id);
            self.bound_program = program.id;
        }
    }

    pub fn create_texture_ids(&mut self,
                              count: i32,
                              target: TextureTarget) -> Vec<TextureId> {
        let id_list = self.gl.gen_textures(count);
        let mut texture_ids = Vec::new();

        for id in id_list {
            let texture_id = TextureId {
                name: id,
                target: target.to_gl_target(),
            };

            let texture = Texture {
                gl: Rc::clone(&self.gl),
                id,
                width: 0,
                height: 0,
                format: ImageFormat::Invalid,
                filter: TextureFilter::Nearest,
                mode: RenderTargetMode::None,
                fbo_ids: vec![],
                depth_rb: None,
            };

            debug_assert!(self.textures.contains_key(&texture_id) == false);
            self.textures.insert(texture_id, texture);

            texture_ids.push(texture_id);
        }

        texture_ids
    }

    pub fn get_texture_dimensions(&self, texture_id: TextureId) -> DeviceUintSize {
        let texture = &self.textures[&texture_id];
        DeviceUintSize::new(texture.width, texture.height)
    }

    fn set_texture_parameters(&mut self, target: gl::GLuint, filter: TextureFilter) {
        let filter = match filter {
            TextureFilter::Nearest => {
                gl::NEAREST
            }
            TextureFilter::Linear => {
                gl::LINEAR
            }
        };

        self.gl.tex_parameter_i(target, gl::TEXTURE_MAG_FILTER, filter as gl::GLint);
        self.gl.tex_parameter_i(target, gl::TEXTURE_MIN_FILTER, filter as gl::GLint);

        self.gl.tex_parameter_i(target, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as gl::GLint);
        self.gl.tex_parameter_i(target, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as gl::GLint);
    }

    fn upload_texture_image(&mut self,
                            target: gl::GLuint,
                            width: u32,
                            height: u32,
                            internal_format: u32,
                            format: u32,
                            type_: u32,
                            pixels: Option<&[u8]>) {
        self.gl.tex_image_2d(target,
                              0,
                              internal_format as gl::GLint,
                              width as gl::GLint, height as gl::GLint,
                              0,
                              format,
                              type_,
                              pixels);
    }

    pub fn init_texture(&mut self,
                        texture_id: TextureId,
                        width: u32,
                        height: u32,
                        format: ImageFormat,
                        filter: TextureFilter,
                        mode: RenderTargetMode,
                        pixels: Option<&[u8]>) {
        debug_assert!(self.inside_frame);

        let resized;
        {
            let texture = self.textures.get_mut(&texture_id).expect("Didn't find texture!");
            texture.format = format;
            resized = texture.width != width || texture.height != height;
            texture.width = width;
            texture.height = height;
            texture.filter = filter;
            texture.mode = mode;
        }

        let (internal_format, gl_format) = gl_texture_formats_for_image_format(self.gl(), format);
        let type_ = gl_type_for_texture_format(format);

        match mode {
            RenderTargetMode::SimpleRenderTarget => {
                self.bind_texture(DEFAULT_TEXTURE, texture_id);
                self.set_texture_parameters(texture_id.target, filter);
                self.upload_texture_image(texture_id.target,
                                          width,
                                          height,
                                          internal_format as u32,
                                          gl_format,
                                          type_,
                                          None);
                self.update_texture_storage(texture_id, None, resized);
            }
            RenderTargetMode::LayerRenderTarget(layer_count) => {
                self.bind_texture(DEFAULT_TEXTURE, texture_id);
                self.set_texture_parameters(texture_id.target, filter);
                self.update_texture_storage(texture_id, Some(layer_count), resized);
            }
            RenderTargetMode::None => {
                self.bind_texture(DEFAULT_TEXTURE, texture_id);
                self.set_texture_parameters(texture_id.target, filter);
                let expanded_data: Vec<u8>;
                let actual_pixels = if pixels.is_some() &&
                                       format == ImageFormat::A8 &&
                                       cfg!(any(target_arch="arm", target_arch="aarch64")) {
                    expanded_data = pixels.unwrap().iter().flat_map(|&byte| repeat(byte).take(4)).collect();
                    Some(expanded_data.as_slice())
                } else {
                    pixels
                };
                self.upload_texture_image(texture_id.target,
                                          width,
                                          height,
                                          internal_format as u32,
                                          gl_format,
                                          type_,
                                          actual_pixels);
            }
        }
    }

    pub fn get_render_target_layer_count(&self, texture_id: TextureId) -> usize {
        self.textures[&texture_id].fbo_ids.len()
    }

    /// Updates the texture storage for the texture, creating
    /// FBOs as required.
    pub fn update_texture_storage(&mut self,
                                  texture_id: TextureId,
                                  layer_count: Option<i32>,
                                  resized: bool) {
        let texture = self.textures.get_mut(&texture_id).unwrap();

        match layer_count {
            Some(layer_count) => {
                assert!(layer_count > 0);
                assert_eq!(texture_id.target, gl::TEXTURE_2D_ARRAY);

                let current_layer_count = texture.fbo_ids.len() as i32;
                // If the texture is already the required size skip.
                if current_layer_count == layer_count && !resized {
                    return;
                }

                let (internal_format, gl_format) = gl_texture_formats_for_image_format(&*self.gl, texture.format);
                let type_ = gl_type_for_texture_format(texture.format);

                self.gl.tex_image_3d(texture_id.target,
                                     0,
                                     internal_format as gl::GLint,
                                     texture.width as gl::GLint,
                                     texture.height as gl::GLint,
                                     layer_count,
                                     0,
                                     gl_format,
                                     type_,
                                     None);

                let needed_layer_count = layer_count - current_layer_count;
                if needed_layer_count > 0 {
                    // Create more framebuffers to fill the gap
                    let new_fbos = self.gl.gen_framebuffers(needed_layer_count);
                    texture.fbo_ids.extend(new_fbos.into_iter().map(|id| FBOId(id)));
                } else if needed_layer_count < 0 {
                    // Remove extra framebuffers
                    for old in texture.fbo_ids.drain(layer_count as usize ..) {
                        self.gl.delete_framebuffers(&[old.0]);
                    }
                }

                let depth_rb = if let Some(rbo) = texture.depth_rb {
                    rbo.0
                } else {
                    let renderbuffer_ids = self.gl.gen_renderbuffers(1);
                    let depth_rb = renderbuffer_ids[0];
                    texture.depth_rb = Some(RBOId(depth_rb));
                    depth_rb
                };
                self.gl.bind_renderbuffer(gl::RENDERBUFFER, depth_rb);
                self.gl.renderbuffer_storage(gl::RENDERBUFFER,
                                             gl::DEPTH_COMPONENT24,
                                             texture.width as gl::GLsizei,
                                             texture.height as gl::GLsizei);

                for (fbo_index, fbo_id) in texture.fbo_ids.iter().enumerate() {
                    self.gl.bind_framebuffer(gl::FRAMEBUFFER, fbo_id.0);
                    self.gl.framebuffer_texture_layer(gl::FRAMEBUFFER,
                                                      gl::COLOR_ATTACHMENT0,
                                                      texture_id.name,
                                                      0,
                                                      fbo_index as gl::GLint);
                    self.gl.framebuffer_renderbuffer(gl::FRAMEBUFFER,
                                                     gl::DEPTH_ATTACHMENT,
                                                     gl::RENDERBUFFER,
                                                     depth_rb);
                }
            }
            None => {
                if texture.fbo_ids.is_empty() {
                    assert!(texture_id.target != gl::TEXTURE_2D_ARRAY);

                    let new_fbo = self.gl.gen_framebuffers(1)[0];
                    self.gl.bind_framebuffer(gl::FRAMEBUFFER, new_fbo);

                    self.gl.framebuffer_texture_2d(gl::FRAMEBUFFER,
                                                   gl::COLOR_ATTACHMENT0,
                                                   texture_id.target,
                                                   texture_id.name,
                                                   0);

                    texture.fbo_ids.push(FBOId(new_fbo));
                } else {
                    assert_eq!(texture.fbo_ids.len(), 1);
                }
            }
        }

        // TODO(gw): Hack! Modify the code above to use the normal binding interfaces the device exposes.
        self.gl.bind_framebuffer(gl::READ_FRAMEBUFFER, self.bound_read_fbo.0);
        self.gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, self.bound_draw_fbo.0);
    }

    pub fn blit_render_target(&mut self,
                              src_texture: Option<(TextureId, i32)>,
                              src_rect: Option<DeviceIntRect>,
                              dest_rect: DeviceIntRect) {
        debug_assert!(self.inside_frame);

        let src_rect = src_rect.unwrap_or_else(|| {
            let texture = self.textures.get(&src_texture.unwrap().0).expect("unknown texture id!");
            DeviceIntRect::new(DeviceIntPoint::zero(),
                               DeviceIntSize::new(texture.width as gl::GLint,
                                                  texture.height as gl::GLint))
        });

        self.bind_read_target(src_texture);

        self.gl.blit_framebuffer(src_rect.origin.x,
                                  src_rect.origin.y,
                                  src_rect.origin.x + src_rect.size.width,
                                  src_rect.origin.y + src_rect.size.height,
                                  dest_rect.origin.x,
                                  dest_rect.origin.y,
                                  dest_rect.origin.x + dest_rect.size.width,
                                  dest_rect.origin.y + dest_rect.size.height,
                                  gl::COLOR_BUFFER_BIT,
                                  gl::LINEAR);
    }

    pub fn resize_texture(&mut self,
                          texture_id: TextureId,
                          new_width: u32,
                          new_height: u32,
                          format: ImageFormat,
                          filter: TextureFilter,
                          mode: RenderTargetMode) {
        debug_assert!(self.inside_frame);

        let old_size = self.get_texture_dimensions(texture_id);

        let temp_texture_id = self.create_texture_ids(1, TextureTarget::Default)[0];
        self.init_texture(temp_texture_id, old_size.width, old_size.height, format, filter, mode, None);
        self.update_texture_storage(temp_texture_id, None, true);

        self.bind_read_target(Some((texture_id, 0)));
        self.bind_texture(DEFAULT_TEXTURE, temp_texture_id);

        self.gl.copy_tex_sub_image_2d(temp_texture_id.target,
                                       0,
                                       0,
                                       0,
                                       0,
                                       0,
                                       old_size.width as i32,
                                       old_size.height as i32);

        self.deinit_texture(texture_id);
        self.init_texture(texture_id, new_width, new_height, format, filter, mode, None);
        self.update_texture_storage(texture_id, None, true);
        self.bind_read_target(Some((temp_texture_id, 0)));
        self.bind_texture(DEFAULT_TEXTURE, texture_id);

        self.gl.copy_tex_sub_image_2d(texture_id.target,
                                       0,
                                       0,
                                       0,
                                       0,
                                       0,
                                       old_size.width as i32,
                                       old_size.height as i32);

        self.bind_read_target(None);
        self.deinit_texture(temp_texture_id);
    }

    pub fn deinit_texture(&mut self, texture_id: TextureId) {
        debug_assert!(self.inside_frame);

        self.bind_texture(DEFAULT_TEXTURE, texture_id);

        let texture = self.textures.get_mut(&texture_id).unwrap();
        let (internal_format, gl_format) = gl_texture_formats_for_image_format(&*self.gl, texture.format);
        let type_ = gl_type_for_texture_format(texture.format);

        self.gl.tex_image_2d(texture_id.target,
                              0,
                              internal_format,
                              0,
                              0,
                              0,
                              gl_format,
                              type_,
                              None);

        if let Some(RBOId(depth_rb)) = texture.depth_rb.take() {
            self.gl.delete_renderbuffers(&[depth_rb]);
        }

        if !texture.fbo_ids.is_empty() {
            let fbo_ids: Vec<_> = texture.fbo_ids.drain(..).map(|FBOId(fbo_id)| fbo_id).collect();
            self.gl.delete_framebuffers(&fbo_ids[..]);
        }

        texture.format = ImageFormat::Invalid;
        texture.width = 0;
        texture.height = 0;
    }

    pub fn create_program(&mut self,
                          base_filename: &str,
                          include_filename: &str,
                          descriptor: &VertexDescriptor) -> Result<Program, ShaderError> {
        self.create_program_with_prefix(base_filename,
                                        &[include_filename],
                                        None,
                                        descriptor)
    }

    pub fn delete_program(&mut self, program: &mut Program) {
        self.gl.delete_program(program.id);
        program.id = 0;
    }

    pub fn create_program_with_prefix(&mut self,
                                      base_filename: &str,
                                      include_filenames: &[&str],
                                      prefix: Option<String>,
                                      descriptor: &VertexDescriptor) -> Result<Program, ShaderError> {
        debug_assert!(self.inside_frame);

        let pid = self.gl.create_program();

        let mut vs_name = String::from(base_filename);
        vs_name.push_str(".vs");
        let mut fs_name = String::from(base_filename);
        fs_name.push_str(".fs");

        let mut include = format!("// Base shader: {}\n", base_filename);
        for inc_filename in include_filenames {
            let src = get_shader_source(inc_filename, &self.resource_override_path);
            include.push_str(&src);
        }

        if let Some(shared_src) = get_optional_shader_source(base_filename, &self.resource_override_path) {
            include.push_str(&shared_src);
        }

        let mut program = Program {
            name: base_filename.to_owned(),
            id: pid,
            u_transform: -1,
            u_device_pixel_ratio: -1,
            vs_source: get_shader_source(&vs_name, &self.resource_override_path),
            fs_source: get_shader_source(&fs_name, &self.resource_override_path),
            prefix,
            vs_id: None,
            fs_id: None,
        };

        try!{ self.load_program(&mut program, include, descriptor) };

        Ok(program)
    }

    fn load_program(&mut self,
                    program: &mut Program,
                    include: String,
                    descriptor: &VertexDescriptor) -> Result<(), ShaderError> {
        debug_assert!(self.inside_frame);

        let mut vs_preamble = Vec::new();
        let mut fs_preamble = Vec::new();

        vs_preamble.push("#define WR_VERTEX_SHADER\n".to_owned());
        fs_preamble.push("#define WR_FRAGMENT_SHADER\n".to_owned());

        if let Some(ref prefix) = program.prefix {
            vs_preamble.push(prefix.clone());
            fs_preamble.push(prefix.clone());
        }

        vs_preamble.push(self.shader_preamble.to_owned());
        fs_preamble.push(self.shader_preamble.to_owned());

        vs_preamble.push(include.clone());
        fs_preamble.push(include);

        // todo(gw): store shader ids so they can be freed!
        let vs_id = try!{ Device::compile_shader(&*self.gl,
                                                 &program.name,
                                                 &program.vs_source,
                                                 gl::VERTEX_SHADER,
                                                 &vs_preamble) };
        let fs_id = try!{ Device::compile_shader(&*self.gl,
                                                 &program.name,
                                                 &program.fs_source,
                                                 gl::FRAGMENT_SHADER,
                                                 &fs_preamble) };

        if let Some(vs_id) = program.vs_id {
            self.gl.detach_shader(program.id, vs_id);
        }

        if let Some(fs_id) = program.fs_id {
            self.gl.detach_shader(program.id, fs_id);
        }

        if let Err(bind_error) = program.attach_and_bind_shaders(vs_id, fs_id, descriptor, &*self.gl) {
            if let (Some(vs_id), Some(fs_id)) = (program.vs_id, program.fs_id) {
                try! { program.attach_and_bind_shaders(vs_id, fs_id, descriptor, &*self.gl) };
            } else {
               return Err(bind_error);
            }
        } else {
            if let Some(vs_id) = program.vs_id {
                self.gl.delete_shader(vs_id);
            }

            if let Some(fs_id) = program.fs_id {
                self.gl.delete_shader(fs_id);
            }

            program.vs_id = Some(vs_id);
            program.fs_id = Some(fs_id);
        }

        program.u_transform = self.gl.get_uniform_location(program.id, "uTransform");
        program.u_device_pixel_ratio = self.gl.get_uniform_location(program.id, "uDevicePixelRatio");

        self.bind_program(program);
        let u_color_0 = self.gl.get_uniform_location(program.id, "sColor0");
        if u_color_0 != -1 {
            self.gl.uniform_1i(u_color_0, TextureSampler::Color0 as i32);
        }
        let u_color1 = self.gl.get_uniform_location(program.id, "sColor1");
        if u_color1 != -1 {
            self.gl.uniform_1i(u_color1, TextureSampler::Color1 as i32);
        }
        let u_color_2 = self.gl.get_uniform_location(program.id, "sColor2");
        if u_color_2 != -1 {
            self.gl.uniform_1i(u_color_2, TextureSampler::Color2 as i32);
        }
        let u_noise = self.gl.get_uniform_location(program.id, "sDither");
        if u_noise != -1 {
            self.gl.uniform_1i(u_noise, TextureSampler::Dither as i32);
        }
        let u_cache_a8 = self.gl.get_uniform_location(program.id, "sCacheA8");
        if u_cache_a8 != -1 {
            self.gl.uniform_1i(u_cache_a8, TextureSampler::CacheA8 as i32);
        }
        let u_cache_rgba8 = self.gl.get_uniform_location(program.id, "sCacheRGBA8");
        if u_cache_rgba8 != -1 {
            self.gl.uniform_1i(u_cache_rgba8, TextureSampler::CacheRGBA8 as i32);
        }

        let u_layers = self.gl.get_uniform_location(program.id, "sLayers");
        if u_layers != -1 {
            self.gl.uniform_1i(u_layers, TextureSampler::Layers as i32);
        }

        let u_tasks = self.gl.get_uniform_location(program.id, "sRenderTasks");
        if u_tasks != -1 {
            self.gl.uniform_1i(u_tasks, TextureSampler::RenderTasks as i32);
        }

        let u_resource_cache = self.gl.get_uniform_location(program.id, "sResourceCache");
        if u_resource_cache != -1 {
            self.gl.uniform_1i(u_resource_cache, TextureSampler::ResourceCache as i32);
        }

        Ok(())
    }

/*
    pub fn refresh_shader(&mut self, path: PathBuf) {
        let mut vs_preamble_path = self.resource_path.clone();
        vs_preamble_path.push(VERTEX_SHADER_PREAMBLE);

        let mut fs_preamble_path = self.resource_path.clone();
        fs_preamble_path.push(FRAGMENT_SHADER_PREAMBLE);

        let mut refresh_all = false;

        if path == vs_preamble_path {
            let mut f = File::open(&vs_preamble_path).unwrap();
            self.vertex_shader_preamble = String::new();
            f.read_to_string(&mut self.vertex_shader_preamble).unwrap();
            refresh_all = true;
        }

        if path == fs_preamble_path {
            let mut f = File::open(&fs_preamble_path).unwrap();
            self.fragment_shader_preamble = String::new();
            f.read_to_string(&mut self.fragment_shader_preamble).unwrap();
            refresh_all = true;
        }

        let mut programs_to_update = Vec::new();

        for (program_id, program) in &mut self.programs {
            if refresh_all || program.vs_path == path || program.fs_path == path {
                programs_to_update.push(*program_id)
            }
        }

        for program_id in programs_to_update {
            self.load_program(program_id, false);
        }
    }*/

    pub fn get_uniform_location(&self, program: &Program, name: &str) -> UniformLocation {
        UniformLocation(self.gl.get_uniform_location(program.id, name))
    }

    pub fn set_uniform_2f(&self, uniform: UniformLocation, x: f32, y: f32) {
        debug_assert!(self.inside_frame);
        let UniformLocation(location) = uniform;
        self.gl.uniform_2f(location, x, y);
    }

    pub fn set_uniforms(&self,
                        program: &Program,
                        transform: &Transform3D<f32>) {
        debug_assert!(self.inside_frame);
        self.gl.uniform_matrix_4fv(program.u_transform,
                                   false,
                                   &transform.to_row_major_array());
        self.gl.uniform_1f(program.u_device_pixel_ratio, self.device_pixel_ratio);
    }

    pub fn create_pbo(&mut self) -> PBOId {
        let id = self.gl.gen_buffers(1)[0];
        PBOId(id)
    }

    pub fn destroy_pbo(&mut self, id: PBOId) {
        self.gl.delete_buffers(&[id.0]);
    }

    pub fn bind_pbo(&mut self, pbo_id: Option<PBOId>) {
        debug_assert!(self.inside_frame);
        let pbo_id = pbo_id.unwrap_or(PBOId(0));

        if self.bound_pbo != pbo_id {
            self.bound_pbo = pbo_id;

            self.gl.bind_buffer(gl::PIXEL_UNPACK_BUFFER, pbo_id.0);
        }
    }

    pub fn update_pbo_data<T>(&mut self, data: &[T]) {
        debug_assert!(self.inside_frame);
        debug_assert!(self.bound_pbo.0 != 0);

        gl::buffer_data(&*self.gl,
                        gl::PIXEL_UNPACK_BUFFER,
                        data,
                        gl::STREAM_DRAW);
    }

    pub fn orphan_pbo(&mut self, new_size: usize) {
        debug_assert!(self.inside_frame);
        debug_assert!(self.bound_pbo.0 != 0);

        self.gl.buffer_data_untyped(gl::PIXEL_UNPACK_BUFFER,
                                    new_size as isize,
                                    ptr::null(),
                                    gl::STREAM_DRAW);
    }

    pub fn update_texture_from_pbo(&mut self,
                                   texture_id: TextureId,
                                   x0: u32,
                                   y0: u32,
                                   width: u32,
                                   height: u32,
                                   offset: usize) {
        debug_assert!(self.inside_frame);
        debug_assert_eq!(self.textures.get(&texture_id).unwrap().format, ImageFormat::RGBAF32);

        self.bind_texture(DEFAULT_TEXTURE, texture_id);

        self.gl.tex_sub_image_2d_pbo(texture_id.target,
                                     0,
                                     x0 as gl::GLint,
                                     y0 as gl::GLint,
                                     width as gl::GLint,
                                     height as gl::GLint,
                                     gl::RGBA,
                                     gl::FLOAT,
                                     offset);
    }

    pub fn update_texture(&mut self,
                          texture_id: TextureId,
                          x0: u32,
                          y0: u32,
                          width: u32,
                          height: u32,
                          stride: Option<u32>,
                          data: &[u8]) {
        debug_assert!(self.inside_frame);

        let mut expanded_data = Vec::new();

        let (gl_format, bpp, data, data_type) = match self.textures.get(&texture_id).unwrap().format {
            ImageFormat::A8 => {
                if cfg!(any(target_arch="arm", target_arch="aarch64")) {
                    expanded_data.extend(data.iter().flat_map(|byte| repeat(*byte).take(4)));
                    (get_gl_format_bgra(self.gl()), 4, expanded_data.as_slice(), gl::UNSIGNED_BYTE)
                } else {
                    (GL_FORMAT_A, 1, data, gl::UNSIGNED_BYTE)
                }
            }
            ImageFormat::RGB8 => (gl::RGB, 3, data, gl::UNSIGNED_BYTE),
            ImageFormat::BGRA8 => (get_gl_format_bgra(self.gl()), 4, data, gl::UNSIGNED_BYTE),
            ImageFormat::RG8 => (gl::RG, 2, data, gl::UNSIGNED_BYTE),
            ImageFormat::RGBAF32 => (gl::RGBA, 16, data, gl::FLOAT),
            ImageFormat::Invalid => unreachable!(),
        };

        let row_length = match stride {
            Some(value) => value / bpp,
            None => width,
        };

        // Take the stride into account for all rows, except the last one.
        let len = bpp * row_length * (height - 1)
                + width * bpp;
        let data = &data[0..len as usize];

        if let Some(..) = stride {
            self.gl.pixel_store_i(gl::UNPACK_ROW_LENGTH, row_length as gl::GLint);
        }

        self.bind_texture(DEFAULT_TEXTURE, texture_id);

        self.gl.tex_sub_image_2d(texture_id.target,
                                 0,
                                 x0 as gl::GLint,
                                 y0 as gl::GLint,
                                 width as gl::GLint,
                                 height as gl::GLint,
                                 gl_format,
                                 data_type,
                                 data);

        // Reset row length to 0, otherwise the stride would apply to all texture uploads.
        if let Some(..) = stride {
            self.gl.pixel_store_i(gl::UNPACK_ROW_LENGTH, 0 as gl::GLint);
        }
    }

    fn clear_vertex_array(&mut self) {
        debug_assert!(self.inside_frame);
        self.gl.bind_vertex_array(0);
    }

    pub fn bind_vao(&mut self, vao_id: VAOId) {
        debug_assert!(self.inside_frame);

        if self.bound_vao != vao_id {
            self.bound_vao = vao_id;

            let VAOId(id) = vao_id;
            self.gl.bind_vertex_array(id);
        }
    }

    fn create_vao_with_vbos(&mut self,
                            descriptor: &VertexDescriptor,
                            main_vbo_id: VBOId,
                            instance_vbo_id: VBOId,
                            ibo_id: IBOId,
                            instance_stride: gl::GLint,
                            owns_vertices: bool,
                            owns_instances: bool,
                            owns_indices: bool)
                            -> VAOId {
        debug_assert!(self.inside_frame);

        let vao_ids = self.gl.gen_vertex_arrays(1);
        let vao_id = vao_ids[0];

        self.gl.bind_vertex_array(vao_id);

        descriptor.bind(self.gl(), main_vbo_id, instance_vbo_id);
        ibo_id.bind(self.gl()); // force it to be a part of VAO

        let vao = VAO {
            gl: Rc::clone(&self.gl),
            id: vao_id,
            ibo_id,
            main_vbo_id,
            instance_vbo_id,
            instance_stride,
            owns_indices,
            owns_vertices,
            owns_instances,
        };

        self.gl.bind_vertex_array(0);

        let vao_id = VAOId(vao_id);

        debug_assert!(!self.vaos.contains_key(&vao_id));
        self.vaos.insert(vao_id, vao);

        vao_id
    }

    pub fn create_vao(&mut self,
                      descriptor: &VertexDescriptor,
                      inst_stride: gl::GLint) -> VAOId {
        debug_assert!(self.inside_frame);

        let buffer_ids = self.gl.gen_buffers(3);
        let ibo_id = IBOId(buffer_ids[0]);
        let main_vbo_id = VBOId(buffer_ids[1]);
        let intance_vbo_id = VBOId(buffer_ids[2]);

        self.create_vao_with_vbos(descriptor,
                                  main_vbo_id,
                                  intance_vbo_id,
                                  ibo_id,
                                  inst_stride,
                                  true,
                                  true,
                                  true)
    }

    pub fn create_vao_with_new_instances(&mut self,
                                         descriptor: &VertexDescriptor,
                                         inst_stride: gl::GLint,
                                         base_vao: VAOId) -> VAOId {
        debug_assert!(self.inside_frame);

        let buffer_ids = self.gl.gen_buffers(1);
        let intance_vbo_id = VBOId(buffer_ids[0]);
        let (main_vbo_id, ibo_id) = {
            let vao = self.vaos.get(&base_vao).unwrap();
            (vao.main_vbo_id, vao.ibo_id)
        };

        self.create_vao_with_vbos(descriptor,
                                  main_vbo_id,
                                  intance_vbo_id,
                                  ibo_id,
                                  inst_stride,
                                  false,
                                  true,
                                  false)
    }

    pub fn update_vao_main_vertices<V>(&mut self,
                                       vao_id: VAOId,
                                       vertices: &[V],
                                       usage_hint: VertexUsageHint) {
        debug_assert!(self.inside_frame);

        let vao = self.vaos.get(&vao_id).unwrap();
        debug_assert_eq!(self.bound_vao, vao_id);

        vao.main_vbo_id.bind(self.gl());
        gl::buffer_data(self.gl(), gl::ARRAY_BUFFER, vertices, usage_hint.to_gl());
    }

    pub fn update_vao_instances<V>(&mut self,
                                   vao_id: VAOId,
                                   instances: &[V],
                                   usage_hint: VertexUsageHint) {
        debug_assert!(self.inside_frame);

        let vao = self.vaos.get(&vao_id).unwrap();
        debug_assert_eq!(self.bound_vao, vao_id);
        debug_assert_eq!(vao.instance_stride as usize, mem::size_of::<V>());

        vao.instance_vbo_id.bind(self.gl());
        gl::buffer_data(self.gl(), gl::ARRAY_BUFFER, instances, usage_hint.to_gl());
    }

    pub fn update_vao_indices<I>(&mut self,
                                 vao_id: VAOId,
                                 indices: &[I],
                                 usage_hint: VertexUsageHint) {
        debug_assert!(self.inside_frame);

        let vao = self.vaos.get(&vao_id).unwrap();
        debug_assert_eq!(self.bound_vao, vao_id);

        vao.ibo_id.bind(self.gl());
        gl::buffer_data(self.gl(), gl::ELEMENT_ARRAY_BUFFER, indices, usage_hint.to_gl());
    }

    pub fn draw_triangles_u16(&mut self, first_vertex: i32, index_count: i32) {
        debug_assert!(self.inside_frame);
        self.gl.draw_elements(gl::TRIANGLES,
                               index_count,
                               gl::UNSIGNED_SHORT,
                               first_vertex as u32 * 2);
    }

    pub fn draw_triangles_u32(&mut self, first_vertex: i32, index_count: i32) {
        debug_assert!(self.inside_frame);
        self.gl.draw_elements(gl::TRIANGLES,
                               index_count,
                               gl::UNSIGNED_INT,
                               first_vertex as u32 * 4);
    }

    pub fn draw_nonindexed_lines(&mut self, first_vertex: i32, vertex_count: i32) {
        debug_assert!(self.inside_frame);
        self.gl.draw_arrays(gl::LINES,
                             first_vertex,
                             vertex_count);
    }

    pub fn draw_indexed_triangles_instanced_u16(&mut self,
                                                index_count: i32,
                                                instance_count: i32) {
        debug_assert!(self.inside_frame);
        self.gl.draw_elements_instanced(gl::TRIANGLES, index_count, gl::UNSIGNED_SHORT, 0, instance_count);
    }

    pub fn end_frame(&mut self) {
        self.bind_draw_target(None, None);
        self.bind_read_target(None);

        debug_assert!(self.inside_frame);
        self.inside_frame = false;

        self.gl.bind_texture(gl::TEXTURE_2D, 0);
        self.gl.use_program(0);

        for i in 0..self.bound_textures.len() {
            self.gl.active_texture(gl::TEXTURE0 + i as gl::GLuint);
            self.gl.bind_texture(gl::TEXTURE_2D, 0);
        }

        self.gl.active_texture(gl::TEXTURE0);

        self.frame_id.0 += 1;
    }

    pub fn clear_target(&self,
                        color: Option<[f32; 4]>,
                        depth: Option<f32>) {
        let mut clear_bits = 0;

        if let Some(color) = color {
            self.gl.clear_color(color[0], color[1], color[2], color[3]);
            clear_bits |= gl::COLOR_BUFFER_BIT;
        }

        if let Some(depth) = depth {
            self.gl.clear_depth(depth as f64);
            clear_bits |= gl::DEPTH_BUFFER_BIT;
        }

        if clear_bits != 0 {
            self.gl.clear(clear_bits);
        }
    }

    pub fn clear_target_rect(&self,
                             color: Option<[f32; 4]>,
                             depth: Option<f32>,
                             rect: DeviceIntRect) {
        let mut clear_bits = 0;

        if let Some(color) = color {
            self.gl.clear_color(color[0], color[1], color[2], color[3]);
            clear_bits |= gl::COLOR_BUFFER_BIT;
        }

        if let Some(depth) = depth {
            self.gl.clear_depth(depth as f64);
            clear_bits |= gl::DEPTH_BUFFER_BIT;
        }

        if clear_bits != 0 {
            self.gl.enable(gl::SCISSOR_TEST);
            self.gl.scissor(rect.origin.x, rect.origin.y, rect.size.width, rect.size.height);
            self.gl.clear(clear_bits);
            self.gl.disable(gl::SCISSOR_TEST);
        }
    }

    pub fn enable_depth(&self) {
        self.gl.enable(gl::DEPTH_TEST);
    }

    pub fn disable_depth(&self) {
        self.gl.disable(gl::DEPTH_TEST);
    }

    pub fn set_depth_func(&self, depth_func: DepthFunction) {
        self.gl.depth_func(depth_func as gl::GLuint);
    }

    pub fn enable_depth_write(&self) {
        self.gl.depth_mask(true);
    }

    pub fn disable_depth_write(&self) {
        self.gl.depth_mask(false);
    }

    pub fn disable_stencil(&self) {
        self.gl.disable(gl::STENCIL_TEST);
    }

    pub fn disable_scissor(&self) {
        self.gl.disable(gl::SCISSOR_TEST);
    }

    pub fn set_blend(&self, enable: bool) {
        if enable {
            self.gl.enable(gl::BLEND);
        } else {
            self.gl.disable(gl::BLEND);
        }
    }

    pub fn set_blend_mode_premultiplied_alpha(&self) {
        self.gl.blend_func(gl::ONE, gl::ONE_MINUS_SRC_ALPHA);
        self.gl.blend_equation(gl::FUNC_ADD);
    }

    pub fn set_blend_mode_alpha(&self) {
        self.gl.blend_func_separate(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA,
                                    gl::ONE, gl::ONE_MINUS_SRC_ALPHA);
        self.gl.blend_equation(gl::FUNC_ADD);
    }

    pub fn set_blend_mode_subpixel(&self, color: ColorF) {
        self.gl.blend_color(color.r, color.g, color.b, color.a);
        self.gl.blend_func(gl::CONSTANT_COLOR, gl::ONE_MINUS_SRC_COLOR);
    }

    pub fn set_blend_mode_multiply(&self) {
        self.gl.blend_func_separate(gl::ZERO, gl::SRC_COLOR,
                                     gl::ZERO, gl::SRC_ALPHA);
        self.gl.blend_equation(gl::FUNC_ADD);
    }
    pub fn set_blend_mode_max(&self) {
        self.gl.blend_func_separate(gl::ONE, gl::ONE,
                                     gl::ONE, gl::ONE);
        self.gl.blend_equation_separate(gl::MAX, gl::FUNC_ADD);
    }
    pub fn set_blend_mode_min(&self) {
        self.gl.blend_func_separate(gl::ONE, gl::ONE,
                                     gl::ONE, gl::ONE);
        self.gl.blend_equation_separate(gl::MIN, gl::FUNC_ADD);
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        //self.file_watcher.exit();
    }
}

/// return (gl_internal_format, gl_format)
fn gl_texture_formats_for_image_format(gl: &gl::Gl, format: ImageFormat) -> (gl::GLint, gl::GLuint) {
    match format {
        ImageFormat::A8 => {
            if cfg!(any(target_arch="arm", target_arch="aarch64")) {
                (get_gl_format_bgra(gl) as gl::GLint, get_gl_format_bgra(gl))
            } else {
                (GL_FORMAT_A as gl::GLint, GL_FORMAT_A)
            }
        },
        ImageFormat::RGB8 => (gl::RGB as gl::GLint, gl::RGB),
        ImageFormat::BGRA8 => {
            match gl.get_type() {
                gl::GlType::Gl =>  {
                    (gl::RGBA as gl::GLint, get_gl_format_bgra(gl))
                }
                gl::GlType::Gles => {
                    (get_gl_format_bgra(gl) as gl::GLint, get_gl_format_bgra(gl))
                }
            }
        }
        ImageFormat::RGBAF32 => (gl::RGBA32F as gl::GLint, gl::RGBA),
        ImageFormat::RG8 => (gl::RG8 as gl::GLint, gl::RG),
        ImageFormat::Invalid => unreachable!(),
    }
}

fn gl_type_for_texture_format(format: ImageFormat) -> gl::GLuint {
    match format {
        ImageFormat::RGBAF32 => gl::FLOAT,
        _ => gl::UNSIGNED_BYTE,
    }
}

