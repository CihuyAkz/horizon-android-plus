// GLES 3.0 Adreno 304 rejects unsigned switch labels. Keep Vulkan's shader
// unchanged and select the same sprite array with ordinary branches on GL.
fn sprite(uv: vec2<f32>, layer: u32) -> vec4<f32> {
    if SPRITE_ARRAYS == 1u {
        return textureSample(sprites, sprite_sampler, uv, i32(layer));
    }
    let dx = dpdx(uv);
    let dy = dpdy(uv);
    let l = i32(layer % SPRITE_SPLIT);
    let array_index = layer / SPRITE_SPLIT;
    if array_index == 0u { return textureSampleGrad(sprites, sprite_sampler, uv, l, dx, dy); }
    if array_index == 1u { return textureSampleGrad(sprites_1, sprite_sampler, uv, l, dx, dy); }
    if array_index == 2u { return textureSampleGrad(sprites_2, sprite_sampler, uv, l, dx, dy); }
    if array_index == 3u { return textureSampleGrad(sprites_3, sprite_sampler, uv, l, dx, dy); }
    if array_index == 4u { return textureSampleGrad(sprites_4, sprite_sampler, uv, l, dx, dy); }
    if array_index == 5u { return textureSampleGrad(sprites_5, sprite_sampler, uv, l, dx, dy); }
    if array_index == 6u { return textureSampleGrad(sprites_6, sprite_sampler, uv, l, dx, dy); }
    return textureSampleGrad(sprites_7, sprite_sampler, uv, l, dx, dy);
}
