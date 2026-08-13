/// Converts a BGRX8888 (DRM `XR24` / evdi's default capture format) buffer
/// into NV12 (Y plane + interleaved UV plane), point-sampling chroma at
/// 4:2:0. BT.601 studio-range integer approximation. Not optimized -- v0
/// scope is correctness for the latency measurement, not conversion speed.
pub fn bgrx_to_nv12(
    src: &[u8],
    width: usize,
    height: usize,
    src_stride: usize,
    y_plane: &mut [u8],
    y_stride: usize,
    uv_plane: &mut [u8],
    uv_stride: usize,
) {
    for row in 0..height {
        let src_row = &src[row * src_stride..];
        let y_row = &mut y_plane[row * y_stride..];
        for col in 0..width {
            let p = col * 4;
            let b = src_row[p] as i32;
            let g = src_row[p + 1] as i32;
            let r = src_row[p + 2] as i32;
            let y = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
            y_row[col] = y.clamp(0, 255) as u8;
        }
    }

    for row in (0..height).step_by(2) {
        let src_row = &src[row * src_stride..];
        let uv_row = &mut uv_plane[(row / 2) * uv_stride..];
        for col in (0..width).step_by(2) {
            let p = col * 4;
            let b = src_row[p] as i32;
            let g = src_row[p + 1] as i32;
            let r = src_row[p + 2] as i32;
            let u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
            let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
            let out = col;
            uv_row[out] = u.clamp(0, 255) as u8;
            uv_row[out + 1] = v.clamp(0, 255) as u8;
        }
    }
}
