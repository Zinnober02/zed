// Rounded-rectangle shader: fill, border and alpha, antialiased.
//
// Local coordinates and half extents are in device pixels relative to the
// quad centre; the signed distance field drives both the outer antialiasing
// and the border band.
struct VertexIn {
    @location(0) position: vec2<f32>,
    @location(1) local: vec2<f32>,
    @location(2) half_size: vec2<f32>,
    @location(3) radii: vec4<f32>,
    @location(4) border: vec2<f32>,
    @location(5) color: vec4<f32>,
    @location(6) border_color: vec4<f32>,
};

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) half_size: vec2<f32>,
    @location(2) radii: vec4<f32>,
    @location(3) border: vec2<f32>,
    @location(4) color: vec4<f32>,
    @location(5) border_color: vec4<f32>,
};

@vertex
fn vs_main(input: VertexIn) -> VertexOut {
    var output: VertexOut;
    output.position = vec4<f32>(input.position, 0.0, 1.0);
    output.local = input.local;
    output.half_size = input.half_size;
    output.radii = input.radii;
    output.border = input.border;
    output.color = input.color;
    output.border_color = input.border_color;
    return output;
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    // Pick the corner radius for the quadrant this fragment is in.
    let top = mix(input.radii.x, input.radii.y, step(0.0, input.local.x));
    let bottom = mix(input.radii.w, input.radii.z, step(0.0, input.local.x));
    let radius = mix(top, bottom, step(0.0, input.local.y));

    let q = abs(input.local) - input.half_size + vec2<f32>(radius, radius);
    let distance = min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0, 0.0))) - radius;

    // One device pixel of antialiasing.
    let aa = 1.0;
    let inside = clamp(0.5 - distance / aa, 0.0, 1.0);

    var color = input.color;
    if (input.border.x > 0.0) {
        let inner = clamp(0.5 - (distance + input.border.x) / aa, 0.0, 1.0);
        let border_mask = clamp(inside - inner, 0.0, 1.0);
        color = mix(input.color, input.border_color, border_mask);
    }
    return vec4<f32>(color.rgb, color.a * inside);
}
