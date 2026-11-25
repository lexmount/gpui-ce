use std::mem;

use anyhow::{Result, anyhow, ensure};
use core_foundation::base::TCFType;
use core_video::{
    metal_texture::{CVMetalTexture, CVMetalTextureGetTexture},
    metal_texture_cache::CVMetalTextureCache,
    pixel_buffer::{
        kCVPixelFormatType_420YpCbCr8Planar as I420,
        kCVPixelFormatType_420YpCbCr8PlanarFullRange as I420_FULL_RANGE,
    },
};
use foreign_types::ForeignTypeRef;
use gpui::{Bounds, PaintSurface, PlanarVideoSurface, ScaledPixels};
use metal::{
    CompileOptions, DeviceRef, MTLBlendFactor, MTLPixelFormat, MTLPrimitiveType,
    MTLSamplerMinMagFilter, RenderCommandEncoderRef, RenderPipelineDescriptor, RenderPipelineState,
    SamplerDescriptor, SamplerState,
};

use super::{DATA_SLOT, SAMPLER_SLOT};

#[repr(C)]
struct Uniforms {
    destination: [f32; 4],
    clip: [f32; 4],
    fade: [f32; 4],
    source: [f32; 4],
    conversion: [[f32; 4]; 3],
    chroma_offset: [f32; 2],
    padding: [f32; 2],
}

pub(super) struct PlanarVideoRenderer {
    pipeline: RenderPipelineState,
    nearest: SamplerState,
    linear: SamplerState,
}

impl PlanarVideoRenderer {
    pub(super) fn new(device: &DeviceRef) -> Self {
        let library = device
            .new_library_with_source(include_str!("planar_video.metal"), &CompileOptions::new())
            .expect("compile planar video shaders");
        let vertex = library
            .get_function("planar_vertex", None)
            .expect("video vertex shader");
        let fragment = library
            .get_function("planar_fragment", None)
            .expect("video fragment shader");
        let pipeline = RenderPipelineDescriptor::new();
        pipeline.set_label("planar video");
        pipeline.set_vertex_function(Some(&vertex));
        pipeline.set_fragment_function(Some(&fragment));
        let attachment = pipeline.color_attachments().object_at(0).unwrap();
        attachment.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        attachment.set_blending_enabled(true);
        attachment.set_source_rgb_blend_factor(MTLBlendFactor::One);
        attachment.set_source_alpha_blend_factor(MTLBlendFactor::One);
        attachment.set_destination_rgb_blend_factor(MTLBlendFactor::OneMinusSourceAlpha);
        attachment.set_destination_alpha_blend_factor(MTLBlendFactor::OneMinusSourceAlpha);
        let descriptor = SamplerDescriptor::new();
        descriptor.set_min_filter(MTLSamplerMinMagFilter::Linear);
        descriptor.set_mag_filter(MTLSamplerMinMagFilter::Linear);
        let linear = device.new_sampler(&descriptor);
        descriptor.set_min_filter(MTLSamplerMinMagFilter::Nearest);
        descriptor.set_mag_filter(MTLSamplerMinMagFilter::Nearest);
        Self {
            pipeline: device
                .new_render_pipeline_state(&pipeline)
                .expect("planar video pipeline"),
            nearest: device.new_sampler(&descriptor),
            linear,
        }
    }

    pub(super) fn draw(
        &self,
        surface: &PaintSurface,
        video: &PlanarVideoSurface,
        cache: &CVMetalTextureCache,
        encoder: &RenderCommandEncoderRef,
    ) -> Result<[CVMetalTexture; 3]> {
        let buffer = &video.buffer;
        ensure!(
            matches!(buffer.get_pixel_format(), I420 | I420_FULL_RANGE)
                && buffer.get_plane_count() == 3,
            "expected a three-plane I420 pixel buffer"
        );
        let [x, y, width, height] = video.visible_rect.map(|value| value as usize);
        ensure!(
            width > 0
                && height > 0
                && x % 2 == 0
                && y % 2 == 0
                && x + width <= buffer.get_width_of_plane(0)
                && y + height <= buffer.get_height_of_plane(0)
                && (1..3).all(|plane| {
                    x / 2 + width.div_ceil(2) <= buffer.get_width_of_plane(plane)
                        && y / 2 + height.div_ceil(2) <= buffer.get_height_of_plane(plane)
                }),
            "invalid planar video visible rect"
        );
        let import = |plane| -> Result<CVMetalTexture> {
            cache
                .create_texture_from_image(
                    buffer.as_concrete_TypeRef(),
                    None,
                    MTLPixelFormat::R8Unorm,
                    buffer.get_width_of_plane(plane),
                    buffer.get_height_of_plane(plane),
                    plane,
                )
                .map_err(|status| anyhow!("CoreVideo plane {plane} import failed ({status})"))
        };
        let planes = [import(0)?, import(1)?, import(2)?];
        let fade = surface.content_mask.fade_out;
        let uniforms = Uniforms {
            destination: bounds_array(surface.bounds),
            clip: bounds_array(surface.content_mask.bounds),
            fade: [fade.left.0, fade.top.0, fade.right.0, fade.bottom.0],
            source: video.visible_rect.map(|value| value as f32),
            conversion: video.color_conversion,
            chroma_offset: video.chroma_offset,
            padding: [0.; 2],
        };
        encoder.set_render_pipeline_state(&self.pipeline);
        let uniforms_ptr = (&raw const uniforms).cast();
        encoder.set_vertex_bytes(DATA_SLOT, mem::size_of::<Uniforms>() as u64, uniforms_ptr);
        encoder.set_fragment_bytes(DATA_SLOT, mem::size_of::<Uniforms>() as u64, uniforms_ptr);
        let sampler = if video.nearest {
            &self.nearest
        } else {
            &self.linear
        };
        encoder.set_fragment_sampler_state(SAMPLER_SLOT, Some(sampler));
        for (index, plane) in planes.iter().enumerate() {
            let texture = unsafe {
                let texture = CVMetalTextureGetTexture(plane.as_concrete_TypeRef());
                ensure!(!texture.is_null(), "missing imported Metal plane");
                metal::TextureRef::from_ptr(texture.cast())
            };
            encoder.set_fragment_texture(index as u64, Some(texture));
        }
        encoder.draw_primitives(MTLPrimitiveType::TriangleStrip, 0, 4);
        Ok(planes)
    }
}

fn bounds_array(bounds: Bounds<ScaledPixels>) -> [f32; 4] {
    [
        bounds.origin.x.0,
        bounds.origin.y.0,
        bounds.size.width.0,
        bounds.size.height.0,
    ]
}
