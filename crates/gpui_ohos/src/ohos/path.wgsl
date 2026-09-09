// Path rasterization: GPUI encodes quadratic curves in the st coordinate.
//
// Straight triangles use st = (0, 1) everywhere, which the shader treats as a
// fully covered fill; curve triangles interpolate st so that the implicit
// curve f = st.x * st.x - st.y gives the signed distance to the curve. The
// screen-space gradient of st is constant per triangle, so it is computed on
// the CPU and passed in (this driver rejects fragment derivatives).
struct VertexIn {
    @location(0) position: vec2<f32>,
    @location(1) st: vec2<f32>,
    @location(2) st_dx: vec2<f32>,
    @location(3) st_dy: vec2<f32>,
    @location(4) color: vec4<f32>,
};

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) st: vec2<f32>,
    @location(1) st_dx: vec2<f32>,
    @location(2) st_dy: vec2<f32>,
    @location(3) color: vec4<f32>,
};

@vertex
fn vs_main(input: VertexIn) -> VertexOut {
    var output: VertexOut;
    output.position = vec4<f32>(input.position, 0.0, 1.0);
    output.st = input.st;
    output.st_dx = input.st_dx;
    output.st_dy = input.st_dy;
    output.color = input.color;
    return output;
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let dx = input.st_dx;
    let dy = input.st_dy;
    var alpha: f32;
    if (length(vec2<f32>(dx.x, dy.x)) < 0.001) {
        // Flat fill: no curve gradient across the triangle.
        alpha = 1.0;
    } else {
        let gradient = 2.0 * input.st.xx * vec2<f32>(dx.x, dy.x) - vec2<f32>(dx.y, dy.y);
        let f = input.st.x * input.st.x - input.st.y;
        let distance = f / length(gradient);
        alpha = clamp(0.5 - distance, 0.0, 1.0);
    }
    return vec4<f32>(input.color.rgb, input.color.a * alpha);
}
