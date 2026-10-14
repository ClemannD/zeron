//! The GPU behind agent visuals.
//!
//! GPUI composites images, not foreign GPU passes, so visuals that need the
//! GPU (fragment shaders, lit 3D scenes) render offscreen here through wgpu
//! (Metal, Vulkan, or Direct3D) into a texture that is read back as
//! straight-alpha BGRA and painted like any other image, clipped, faded, and
//! layered with the transcript. One device serves every visual; pipelines are
//! cached by source, so a shader compiles once however many frames it draws.

use std::sync::OnceLock;

mod scene;
mod shader;

pub use scene::render_scene;
pub use shader::{ShaderSpec, ShaderState, render_shader};

/// One rendered frame: straight-alpha BGRA, top row first.
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub(crate) struct Gpu {
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) shaders: OnceLock<shader::Pipelines>,
    pub(crate) scene: OnceLock<scene::Pipelines>,
}

/// The shared device, made on first use; `None` without a usable adapter.
pub(crate) fn gpu() -> Option<&'static Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| match pollster::block_on(Gpu::new()) {
        Ok(gpu) => Some(gpu),
        Err(error) => {
            tracing::warn!(%error, "visual GPU unavailable");
            None
        }
    })
    .as_ref()
}

/// Whether visuals can render on the GPU here.
pub fn available() -> bool {
    gpu().is_some()
}

/// The target every visual renders into before readback.
pub(crate) const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

impl Gpu {
    async fn new() -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            flags: wgpu::InstanceFlags::default(),
            backend_options: wgpu::BackendOptions::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            display: None,
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
            .map_err(|e| e.to_string())?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("zeron visuals"),
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self {
            device,
            queue,
            shaders: OnceLock::new(),
            scene: OnceLock::new(),
        })
    }

/// Read a rendered texture back as straight-alpha BGRA.
    pub(crate) fn read_back(
        &self,
        mut encoder: wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        opaque: bool,
    ) -> Frame {
        let (width, height) = (texture.width(), texture.height());
        let row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("visual readback"),
            size: u64::from(row * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let mut bgra = vec![0u8; (width * height * 4) as usize];
        if let Ok(Ok(())) = rx.recv() {
            let mapped = buffer.slice(..).get_mapped_range();
            for y in 0..height as usize {
                let src = &mapped[y * row as usize..][..width as usize * 4];
                let dst = &mut bgra[y * width as usize * 4..][..width as usize * 4];
                for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                    d[0] = s[2];
                    d[1] = s[1];
                    d[2] = s[0];
                    d[3] = if opaque { 255 } else { s[3] };
                }
            }
            drop(mapped);
            buffer.unmap();
        }
        Frame {
            width,
            height,
            bgra,
        }
    }

    pub(crate) fn target(&self, width: u32, height: u32, label: &str) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }
}

#[cfg(test)]
pub(crate) fn dump_ppm(frame: &Frame, path: &str) {
    let mut out = format!("P6\n{} {}\n255\n", frame.width, frame.height).into_bytes();
    for p in frame.bgra.chunks_exact(4) {
        // Over a mid grey, so transparency reads.
        let a = f32::from(p[3]) / 255.0;
        for ch in [p[2], p[1], p[0]] {
            out.push((f32::from(ch) * a + 60.0 * (1.0 - a)) as u8);
        }
    }
    let _ = std::fs::write(path, out);
}
