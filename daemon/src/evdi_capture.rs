//! evdi connect / event-loop, mirroring the already-proven Milestone 1 C
//! test client (experiments/evdi-bringup/evdi_test_client.c), but feeding
//! each captured frame into the VAAPI encoder instead of just logging it.

use crate::color_convert::bgrx_to_nv12;
use crate::ffi;
use crate::vaapi_encoder::VaapiEncoder;
use std::fs::File;
use std::io::Write;
use std::os::raw::c_void;
use std::time::{Duration, Instant};

struct CaptureState {
    handle: ffi::evdi_handle,
    buffer_id: i32,
    buffer: Option<ffi::evdi_buffer>,
    rects: Vec<ffi::evdi_rect>,
    rgb_data: Vec<u8>,
    y_plane: Vec<u8>,
    uv_plane: Vec<u8>,
    encoder: Option<VaapiEncoder>,
    out_file: File,
    durations: Vec<Duration>,
    frame_count: u64,
}

pub struct CaptureStats {
    pub frame_count: u64,
    pub durations: Vec<Duration>,
}

extern "C" fn mode_changed_handler(mode: ffi::evdi_mode, user_data: *mut c_void) {
    let state = unsafe { &mut *(user_data as *mut CaptureState) };
    eprintln!(
        "[evdi] mode_changed: {}x{} @{}Hz bpp={}",
        mode.width, mode.height, mode.refresh_rate, mode.bits_per_pixel
    );

    // mode_changed can fire more than once per connection (e.g. a DPMS
    // on/off/on cycle) -- unregister the previous buffer before
    // re-registering the same id, or evdi_lib asserts and aborts.
    if state.buffer.is_some() {
        unsafe { ffi::evdi_unregister_buffer(state.handle, state.buffer_id) };
        state.buffer = None;
    }

    let width = mode.width as u32;
    let height = mode.height as u32;
    let stride = width as usize * 4;

    state.rgb_data = vec![0u8; stride * height as usize];
    state.rects = vec![ffi::evdi_rect::default(); 16];

    let encoder = VaapiEncoder::new(width, height).expect("VAAPI encoder init failed");
    let aw = encoder.aligned_width() as usize;
    let ah = encoder.aligned_height() as usize;
    state.y_plane = vec![0u8; aw * ah];
    state.uv_plane = vec![0u8; aw * (ah / 2)];
    state.encoder = Some(encoder);

    state.buffer = Some(ffi::evdi_buffer {
        id: state.buffer_id,
        buffer: state.rgb_data.as_mut_ptr() as *mut c_void,
        width: width as i32,
        height: height as i32,
        stride: stride as i32,
        rects: state.rects.as_mut_ptr(),
        rect_count: state.rects.len() as i32,
    });
    unsafe {
        ffi::evdi_register_buffer(state.handle, state.buffer.unwrap());
        ffi::evdi_request_update(state.handle, state.buffer_id);
    }
}

extern "C" fn update_ready_handler(_buffer_to_be_updated: i32, user_data: *mut c_void) {
    let state = unsafe { &mut *(user_data as *mut CaptureState) };
    let mut num_rects = state.rects.len() as i32;

    let start = Instant::now();

    unsafe {
        ffi::evdi_grab_pixels(state.handle, state.rects.as_mut_ptr(), &mut num_rects);
    }
    // Re-request immediately, before any conversion/encode/file-I/O work,
    // so our own processing time doesn't add latency to the next flip --
    // same lesson learned from the Milestone 1 evdi_test_client.
    unsafe {
        ffi::evdi_request_update(state.handle, state.buffer_id);
    }

    let Some(encoder) = state.encoder.as_mut() else {
        return;
    };
    let width = encoder.width() as usize;
    let height = encoder.height() as usize;
    let aligned_width = encoder.aligned_width() as usize;
    let stride = width * 4;

    bgrx_to_nv12(
        &state.rgb_data,
        width,
        height,
        stride,
        &mut state.y_plane,
        aligned_width,
        &mut state.uv_plane,
        aligned_width,
    );

    match encoder.encode_frame(&state.y_plane, &state.uv_plane) {
        Ok(bytes) => {
            let elapsed = start.elapsed();
            state.frame_count += 1;
            state.durations.push(elapsed);
            if state.frame_count == 1 || state.frame_count % 30 == 0 {
                eprintln!(
                    "[capture] frame {}: {} bytes, {:?} (grab->encoded), dirty_rects={}",
                    state.frame_count,
                    bytes.len(),
                    elapsed,
                    num_rects
                );
            }
            let _ = state.out_file.write_all(&bytes);
        }
        Err(e) => eprintln!("[capture] encode_frame error: {e}"),
    }
}

extern "C" fn dpms_handler(dpms_mode: i32, _user_data: *mut c_void) {
    eprintln!("[evdi] dpms mode={dpms_mode}");
}

extern "C" fn crtc_state_handler(state: i32, _user_data: *mut c_void) {
    eprintln!("[evdi] crtc_state={state}");
}

pub fn run(card: i32, out_path: &str, stop: &std::sync::atomic::AtomicBool) -> CaptureStats {
    use std::sync::atomic::Ordering;

    let status = unsafe { ffi::evdi_check_device(card) };
    if status != ffi::evdi_device_status_AVAILABLE {
        panic!("card{card} is not an available evdi device (status={status})");
    }

    let handle = unsafe { ffi::evdi_open(card) };
    if handle.is_null() {
        panic!("evdi_open({card}) failed");
    }

    let edid = std::fs::read("/sys/class/drm/card0-eDP-1/edid").expect("read eDP EDID");

    let out_file = File::create(out_path).expect("create output file");

    let mut state = Box::new(CaptureState {
        handle,
        buffer_id: 1,
        buffer: None,
        rects: Vec::new(),
        rgb_data: Vec::new(),
        y_plane: Vec::new(),
        uv_plane: Vec::new(),
        encoder: None,
        out_file,
        durations: Vec::new(),
        frame_count: 0,
    });

    unsafe {
        ffi::evdi_connect(handle, edid.as_ptr(), edid.len() as u32, 0);
    }
    eprintln!("[evdi] connected to card{card}, waiting for mode change...");

    let mut ctx = ffi::evdi_event_context {
        dpms_handler: Some(dpms_handler),
        mode_changed_handler: Some(mode_changed_handler),
        update_ready_handler: Some(update_ready_handler),
        crtc_state_handler: Some(crtc_state_handler),
        cursor_set_handler: None,
        cursor_move_handler: None,
        ddcci_data_handler: None,
        user_data: state.as_mut() as *mut CaptureState as *mut c_void,
    };

    let fd = unsafe { ffi::evdi_get_event_ready(handle) };

    while !stop.load(Ordering::SeqCst) {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, 1000) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            eprintln!("[evdi] poll error: {err}");
            break;
        }
        if rc > 0 && (pfd.revents & libc::POLLIN) != 0 {
            unsafe { ffi::evdi_handle_events(handle, &mut ctx) };
        }
    }

    eprintln!("[evdi] shutting down...");
    if state.buffer.is_some() {
        unsafe { ffi::evdi_unregister_buffer(handle, state.buffer_id) };
    }
    unsafe {
        ffi::evdi_disconnect(handle);
        ffi::evdi_close(handle);
    }

    CaptureStats {
        frame_count: state.frame_count,
        durations: state.durations,
    }
}
