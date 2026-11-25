#include <metal_stdlib>
using namespace metal;

struct PlanarUniforms {
    float4 destination;
    float4 clip;
    float4 fade;
    float4 source;
    float4 red;
    float4 green;
    float4 blue;
    float2 chroma_offset;
    float2 padding;
};

struct PlanarVertex {
    float4 position [[position]];
    float2 uv;
};

vertex PlanarVertex planar_vertex(
    uint vertex_id [[vertex_id]],
    constant float2& viewport_size [[buffer(0)]],
    constant PlanarUniforms& video [[buffer(2)]]
) {
    float2 uv = float2(vertex_id & 1, vertex_id >> 1);
    float2 position = (video.destination.xy + uv * video.destination.zw) / viewport_size;
    return {float4(position * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0), uv};
}

fragment float4 planar_fragment(
    PlanarVertex input [[stage_in]],
    constant PlanarUniforms& video [[buffer(2)]],
    texture2d<float> luma [[texture(0)]],
    texture2d<float> cb [[texture(1)]],
    texture2d<float> cr [[texture(2)]],
    sampler plane_sampler [[sampler(0)]]
) {
    float2 position = input.position.xy - video.clip.xy;
    if (any(position < 0.0) || any(position >= video.clip.zw)) {
        return float4(0.0);
    }
    float4 distances = float4(position, video.clip.zw - position);
    float4 coverage = select(float4(1.0), saturate(distances / max(video.fade, 0.0001)), video.fade > 0.0);
    float alpha = coverage.x * coverage.y * coverage.z * coverage.w;

    float2 source = video.source.xy + input.uv * video.source.zw;
    float2 y = clamp(source, video.source.xy + 0.5, video.source.xy + video.source.zw - 0.5);
    float2 chroma_min = video.source.xy * 0.5;
    float2 chroma_max = chroma_min + ceil(video.source.zw * 0.5);
    float2 uv = clamp(source * 0.5 + video.chroma_offset, chroma_min + 0.5, chroma_max - 0.5);
    float4 color(
        luma.sample(plane_sampler, y / float2(luma.get_width(), luma.get_height()), level(0)).r,
        cb.sample(plane_sampler, uv / float2(cb.get_width(), cb.get_height()), level(0)).r,
        cr.sample(plane_sampler, uv / float2(cr.get_width(), cr.get_height()), level(0)).r,
        1.0
    );
    float3 rgb = saturate(float3(dot(video.red, color), dot(video.green, color), dot(video.blue, color)));
    return float4(rgb * alpha, alpha);
}
