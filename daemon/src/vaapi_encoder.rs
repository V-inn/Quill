//! VAAPI H.264 encoder with a real IPPP GOP: one IDR every `GOP_SIZE` frames,
//! plain single-reference P slices in between. Each P slice references only
//! the immediately preceding frame (`max_num_ref_frames = 1`), so a 2-surface
//! ping-pong DPB is enough -- no long-term reference bookkeeping needed.
//! SPS/PPS packed headers are only injected on IDR frames, matching normal
//! Annex-B convention (a P slice's decoder already holds the active
//! parameter sets from the last IDR).

use crate::ffi;
use crate::h264_headers::{self, H264Params};
use std::ffi::c_void;
use std::ptr;

pub type VaResult<T> = Result<T, String>;

/// One encoded frame plus whether it's an IDR. The flag travels to the Android
/// client in the frame header so it can pass `BUFFER_FLAG_KEY_FRAME` only when
/// it's actually true -- before the GOP landed every frame was an IDR and the
/// client hardcoded the flag, which has been a lie on every P frame since.
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub is_idr: bool,
}

/// One PipeWire DMA-BUF plane, as handed over by KWin's screencast. Single
/// plane in practice for the packed BGRx format this stream negotiates.
pub struct DmabufPlane {
    pub fd: std::os::fd::RawFd,
    pub offset: u32,
    pub stride: u32,
    pub size: u32,
    pub modifier: u64,
}

/// `DRM_FORMAT_XRGB8888` -- little-endian 0xXXRRGGBB, i.e. B,G,R,X in memory
/// order, which is exactly what SPA calls BGRx and what `VA_FOURCC_BGRX`
/// expects. The three names describe the same byte layout.
const DRM_FORMAT_XRGB8888: u32 = u32::from_le_bytes([b'X', b'R', b'2', b'4']);

fn check(status: ffi::VAStatus, what: &str) -> VaResult<()> {
    if status as u32 == ffi::VA_STATUS_SUCCESS {
        Ok(())
    } else {
        Err(format!("{what} failed: VAStatus={status}"))
    }
}

fn align16(v: u32) -> u32 {
    (v + 15) & !15
}

/// Walks the driver's coded-buffer segment list into one contiguous buffer.
///
/// The list is written by the VA driver inside the buffer Quill allocated, and
/// until this was bounded, nothing validated it on the way back out: `size` was
/// used directly as a slice length and `next` was chased until it happened to
/// be null. A driver reporting an inflated `size` would copy adjacent process
/// memory into the frame that then goes to the phone; a cyclic `next` would
/// loop until the allocator gave up. Both are now bounded against `cap` -- the
/// size the allocation actually asked for -- and a segment budget.
///
/// # Safety
/// `buf_ptr` must be the live mapping returned by `vaMapBuffer` for a coded
/// buffer of at most `cap` bytes, valid for the duration of this call.
unsafe fn collect_coded_segments(buf_ptr: *mut c_void, cap: usize) -> VaResult<Vec<u8>> {
    // This encoder emits one segment per frame in practice. A list longer than
    // this is malformed, not a bitstream.
    const MAX_SEGMENTS: usize = 64;

    let mut out: Vec<u8> = Vec::new();
    let mut seg = buf_ptr as *const ffi::VACodedBufferSegment;
    let mut count = 0usize;

    while !seg.is_null() {
        count += 1;
        if count > MAX_SEGMENTS {
            return Err(format!(
                "coded buffer: segment list still going after {MAX_SEGMENTS} segments -- \
                 refusing to follow it further"
            ));
        }
        let s = &*seg;
        if s.buf.is_null() {
            return Err(format!("coded buffer: segment {count} has a null data pointer"));
        }
        let size = s.size as usize;
        if size > cap || out.len() + size > cap {
            return Err(format!(
                "coded buffer: segment {count} reports {size} bytes, which with the {} already \
                 read overruns the {cap}-byte buffer that was allocated",
                out.len()
            ));
        }
        out.extend_from_slice(std::slice::from_raw_parts(s.buf as *const u8, size));
        seg = s.next as *const ffi::VACodedBufferSegment;
    }
    Ok(out)
}

/// Byte offset and length of plane 0, validated against the buffer the driver
/// says it actually mapped.
///
/// A `VAImage` carries `data_size` -- the real size of the mapping -- right
/// next to the `offsets`/`pitches` that describe where the plane sits inside
/// it. Nothing in the API forces those three to agree, so a slice built from
/// `offsets`/`pitches` alone is sound only for as long as the driver is
/// well-behaved. `vaDeriveImage` is the trust boundary here: the values come
/// back from the VA driver (iHD, in practice), not from anything Quill
/// computed. Checking costs two comparisons per frame and turns a potential
/// out-of-bounds read *or write* into a returned error.
fn plane0_extent(image: &ffi::VAImage, rows: u32, what: &str) -> VaResult<(usize, usize)> {
    let (offset, pitch) = (image.offsets[0], image.pitches[0]);
    let len = pitch
        .checked_mul(rows)
        .ok_or_else(|| format!("{what}: pitch {pitch} x {rows} rows overflows u32"))?;
    let end = offset
        .checked_add(len)
        .ok_or_else(|| format!("{what}: offset {offset} + length {len} overflows u32"))?;
    if end > image.data_size {
        return Err(format!(
            "{what}: plane 0 ends at byte {end} but the driver mapped only {} -- \
             refusing to touch memory past the end of the buffer",
            image.data_size
        ));
    }
    Ok((offset as usize, len as usize))
}

/// Frames between IDRs (inclusive of the IDR itself). At the fixed 60fps
/// hardware decode path (see MILESTONES.md), this is one IDR per second --
/// tune down if the USB link needs faster recovery after a dropped frame,
/// up for more bandwidth headroom. Must stay comfortably under the
/// `log2_max_frame_num_minus4`/`log2_max_pic_order_cnt_lsb_minus4` wrap
/// points set in `encode_frame` (256 / 512).
const GOP_SIZE: u64 = 60;

pub struct VaapiEncoder {
    dpy: ffi::VADisplay,
    render_fd: std::os::raw::c_int,
    config_id: ffi::VAConfigID,
    context_id: ffi::VAContextID,
    // Ping-pong DPB: 2 NV12 surfaces is enough for max_num_ref_frames=1 --
    // each P slice's sole reference is the other slot, which still holds
    // the previous frame's reconstructed picture untouched.
    surfaces: [ffi::VASurfaceID; 2],
    // GPU color conversion (Milestone 7 follow-up): a BGRX source surface,
    // uploaded via a straight memcpy (no per-pixel math), converted to the
    // NV12 `surface` above by VAAPI's own Video Post-Processing (VPP)
    // entrypoint instead of the CPU scalar BT.601 loop in color_convert.rs.
    // Replaced ~10ms of CPU time (more than the hardware encode itself
    // took) -- see MILESTONES.md for the measured before/after.
    src_surface: ffi::VASurfaceID,
    /// Zero-copy path: PipeWire DMA-BUF fds imported as VA surfaces, keyed by
    /// fd. KWin hands out a small fixed pool (2-4 buffers) and reuses those
    /// same fds for the life of the stream, so importing once per fd rather
    /// than once per frame turns a per-frame `vaCreateSurfaces` into a hash
    /// lookup. Destroyed together in `Drop`.
    imported_surfaces: std::collections::HashMap<std::os::fd::RawFd, ffi::VASurfaceID>,
    vpp_config_id: ffi::VAConfigID,
    vpp_context_id: ffi::VAContextID,
    /// Encoder *output* geometry -- what goes on the wire. At a quarter turn
    /// this is the transpose of the capture, which is the one place rotation
    /// stops being a filter and starts changing shape.
    width: u32,
    height: u32,
    aligned_width: u32,
    aligned_height: u32,
    /// Capture geometry: the size of the frames arriving from PipeWire, and so
    /// the size of the BGRX surface they are uploaded into. Equal to the output
    /// geometry except at a quarter turn.
    src_width: u32,
    src_height: u32,
    src_aligned_width: u32,
    src_aligned_height: u32,
    // Milestone 16: KWin's rotation property has no effect on what a
    // krfb-virtualmonitor output's screencast producer actually exports --
    // confirmed live, toggling it (even via System Settings directly, not
    // just our own automation) changed kscreen-doctor's reported metadata
    // but never the captured pixels. VPP's own rotation_state is the
    // GPU-accelerated place that does work, applied here instead.
    rotation: crate::protocol::Rotation,
    quality: crate::protocol::Quality,
    // GOP state: total frames encoded so far (drives the ping-pong slot and
    // the IDR/P decision), a counter distinguishing successive IDRs
    // (idr_pic_id must differ between them even though frame_num resets to
    // 0 each time), and the previous frame's frame_num/POC -- needed to
    // populate the P slice's single reference-picture entry.
    /// Encoder speed/quality preset, queried from
    /// `VAConfigAttribEncQualityRange` at init. VAAPI's convention is 1 = best
    /// quality, higher = faster; this is pinned to the driver's advertised
    /// maximum. A virtual monitor is a latency problem, not an archival one,
    /// and the rate control is CQP so picture quality is set by QP regardless.
    quality_level: u32,
    frame_count: u64,
    idr_count: u16,
    prev_frame_num: u16,
    prev_poc: i32,
}

impl VaapiEncoder {
    /// `src_width`/`src_height` are the captured frame's dimensions. The
    /// encoder's own output is those dimensions transposed when `rotation` is a
    /// quarter turn, and identical otherwise.
    pub fn new(
        src_width: u32,
        src_height: u32,
        rotation: crate::protocol::Rotation,
        quality: crate::protocol::Quality,
    ) -> VaResult<Self> {
        let (width, height) = if rotation.swaps_axes() {
            (src_height, src_width)
        } else {
            (src_width, src_height)
        };
        let render_fd = unsafe {
            libc::open(
                c"/dev/dri/renderD128".as_ptr(),
                libc::O_RDWR,
            )
        };
        if render_fd < 0 {
            return Err("failed to open /dev/dri/renderD128".into());
        }

        let dpy = unsafe { ffi::vaGetDisplayDRM(render_fd) };
        if dpy.is_null() {
            unsafe { libc::close(render_fd) };
            return Err("vaGetDisplayDRM returned null".into());
        }

        let mut major = 0;
        let mut minor = 0;
        check(
            unsafe { ffi::vaInitialize(dpy, &mut major, &mut minor) },
            "vaInitialize",
        )?;
        eprintln!("[vaapi] initialized, version {major}.{minor}");

        // Query whether the driver wants us to supply SPS/PPS as packed
        // headers (the iHD LP entrypoint does -- see h264_headers.rs).
        let mut packed_headers_attrib = ffi::VAConfigAttrib {
            type_: ffi::VAConfigAttribType_VAConfigAttribEncPackedHeaders,
            value: 0,
        };
        check(
            unsafe {
                ffi::vaGetConfigAttributes(
                    dpy,
                    ffi::VAProfile_VAProfileH264Main,
                    ffi::VAEntrypoint_VAEntrypointEncSliceLP,
                    &mut packed_headers_attrib,
                    1,
                )
            },
            "vaGetConfigAttributes",
        )?;
        let packed_headers_supported = packed_headers_attrib.value
            & (ffi::VA_ENC_PACKED_HEADER_SEQUENCE | ffi::VA_ENC_PACKED_HEADER_PICTURE);
        eprintln!(
            "[vaapi] packed header support bitmask: {:#x} (requesting: {:#x})",
            packed_headers_attrib.value, packed_headers_supported
        );

        let mut quality_attrib = ffi::VAConfigAttrib {
            type_: ffi::VAConfigAttribType_VAConfigAttribEncQualityRange,
            value: 0,
        };
        check(
            unsafe {
                ffi::vaGetConfigAttributes(
                    dpy,
                    ffi::VAProfile_VAProfileH264Main,
                    ffi::VAEntrypoint_VAEntrypointEncSliceLP,
                    &mut quality_attrib,
                    1,
                )
            },
            "vaGetConfigAttributes(quality range)",
        )?;
        // VA_ATTRIB_NOT_SUPPORTED comes back as all-ones; anything else is the
        // number of levels, of which the highest is the fastest.
        let quality_level = if quality_attrib.value == 0 || quality_attrib.value == u32::MAX {
            0 // 0 means "driver default", i.e. don't send the buffer at all
        } else {
            // 1 is the driver's best, `value` its fastest. The preset picks a
            // fraction along that range rather than a fixed number, since the
            // range is driver-specific -- iHD reports something quite different
            // from AMD, and a hardcoded level would mean opposite things.
            let span = quality_attrib.value as f32;
            (span * quality.speed_fraction()).round().clamp(1.0, span) as u32
        };
        eprintln!(
            "[vaapi] encoder quality range: {} (using level {quality_level}, {:?} at {} Mbps)",
            quality_attrib.value,
            quality,
            quality.bits_per_second() / 1_000_000,
        );

        let mut attribs = [
            ffi::VAConfigAttrib {
                type_: ffi::VAConfigAttribType_VAConfigAttribRTFormat,
                value: ffi::VA_RT_FORMAT_YUV420,
            },
            ffi::VAConfigAttrib {
                type_: ffi::VAConfigAttribType_VAConfigAttribRateControl,
                value: ffi::VA_RC_CQP,
            },
            ffi::VAConfigAttrib {
                type_: ffi::VAConfigAttribType_VAConfigAttribEncPackedHeaders,
                value: packed_headers_supported,
            },
        ];
        let mut config_id: ffi::VAConfigID = 0;
        check(
            unsafe {
                ffi::vaCreateConfig(
                    dpy,
                    ffi::VAProfile_VAProfileH264Main,
                    ffi::VAEntrypoint_VAEntrypointEncSliceLP,
                    attribs.as_mut_ptr(),
                    attribs.len() as i32,
                    &mut config_id,
                )
            },
            "vaCreateConfig",
        )?;

        let aligned_width = align16(width);
        let aligned_height = align16(height);
        let src_aligned_width = align16(src_width);
        let src_aligned_height = align16(src_height);

        let mut pixel_format_attrib = ffi::VASurfaceAttrib {
            type_: ffi::VASurfaceAttribType_VASurfaceAttribPixelFormat,
            flags: ffi::VA_SURFACE_ATTRIB_SETTABLE,
            value: ffi::VAGenericValue {
                type_: ffi::VAGenericValueType_VAGenericValueTypeInteger,
                value: ffi::_VAGenericValue__bindgen_ty_1 { i: ffi::VA_FOURCC_NV12 as i32 },
            },
        };
        let mut surfaces: [ffi::VASurfaceID; 2] = [0, 0];
        check(
            unsafe {
                ffi::vaCreateSurfaces(
                    dpy,
                    ffi::VA_RT_FORMAT_YUV420,
                    aligned_width,
                    aligned_height,
                    surfaces.as_mut_ptr(),
                    surfaces.len() as u32,
                    &mut pixel_format_attrib,
                    1,
                )
            },
            "vaCreateSurfaces",
        )?;

        let mut context_id: ffi::VAContextID = 0;
        let mut render_targets = surfaces;
        check(
            unsafe {
                ffi::vaCreateContext(
                    dpy,
                    config_id,
                    aligned_width as i32,
                    aligned_height as i32,
                    0, // no VA_PROGRESSIVE flag needed for encode
                    render_targets.as_mut_ptr(),
                    render_targets.len() as i32,
                    &mut context_id,
                )
            },
            "vaCreateContext",
        )?;

        // GPU color conversion setup: a second surface in the source BGRX
        // format, and a VPP (Video Post-Processing) config/context to
        // convert it into whichever `surfaces` slot is the current target,
        // entirely on the iGPU.
        let mut bgrx_format_attrib = ffi::VASurfaceAttrib {
            type_: ffi::VASurfaceAttribType_VASurfaceAttribPixelFormat,
            flags: ffi::VA_SURFACE_ATTRIB_SETTABLE,
            value: ffi::VAGenericValue {
                type_: ffi::VAGenericValueType_VAGenericValueTypeInteger,
                value: ffi::_VAGenericValue__bindgen_ty_1 { i: ffi::VA_FOURCC_BGRX as i32 },
            },
        };
        let mut src_surface: ffi::VASurfaceID = 0;
        check(
            unsafe {
                ffi::vaCreateSurfaces(
                    dpy,
                    ffi::VA_RT_FORMAT_RGB32,
                    // Capture-shaped, not output-shaped: this is what the
                    // frames from PipeWire are uploaded into, and the VPP pass
                    // is what turns them.
                    src_aligned_width,
                    src_aligned_height,
                    &mut src_surface,
                    1,
                    &mut bgrx_format_attrib,
                    1,
                )
            },
            "vaCreateSurfaces(BGRX source)",
        )?;

        let mut vpp_config_id: ffi::VAConfigID = 0;
        check(
            unsafe {
                ffi::vaCreateConfig(
                    dpy,
                    ffi::VAProfile_VAProfileNone,
                    ffi::VAEntrypoint_VAEntrypointVideoProc,
                    ptr::null_mut(),
                    0,
                    &mut vpp_config_id,
                )
            },
            "vaCreateConfig(VPP)",
        )?;

        let mut vpp_context_id: ffi::VAContextID = 0;
        let mut vpp_render_targets = surfaces;
        check(
            unsafe {
                ffi::vaCreateContext(
                    dpy,
                    vpp_config_id,
                    aligned_width as i32,
                    aligned_height as i32,
                    0,
                    vpp_render_targets.as_mut_ptr(),
                    vpp_render_targets.len() as i32,
                    &mut vpp_context_id,
                )
            },
            "vaCreateContext(VPP)",
        )?;

        // Which rotations this driver's VPP will actually do. Queried rather
        // than assumed: quarter turns are a separate capability from the
        // 180-degree flip, and finding out at runtime beats finding out from a
        // black screen.
        let mut caps: ffi::VAProcPipelineCaps = unsafe { std::mem::zeroed() };
        let caps_status = unsafe {
            ffi::vaQueryVideoProcPipelineCaps(
                dpy,
                vpp_context_id,
                std::ptr::null_mut(),
                0,
                &mut caps,
            )
        };
        let rotation_flags = if caps_status == ffi::VA_STATUS_SUCCESS as i32 {
            caps.rotation_flags
        } else {
            0
        };
        eprintln!(
            "[vaapi] VPP rotation_flags=0x{rotation_flags:x} (none={} 90={} 180={} 270={})",
            rotation_flags & (1 << ffi::VA_ROTATION_NONE) != 0,
            rotation_flags & (1 << ffi::VA_ROTATION_90) != 0,
            rotation_flags & (1 << ffi::VA_ROTATION_180) != 0,
            rotation_flags & (1 << ffi::VA_ROTATION_270) != 0,
        );

        eprintln!(
            "[vaapi] encoder ready: capture {src_width}x{src_height} -> output {width}x{height} \
             (aligned {aligned_width}x{aligned_height}), rotation {}deg, GPU color conversion via VPP, GOP {GOP_SIZE}",
            rotation.degrees()
        );

        Ok(Self {
            dpy,
            render_fd,
            config_id,
            context_id,
            surfaces,
            src_surface,
            imported_surfaces: std::collections::HashMap::new(),
            vpp_config_id,
            vpp_context_id,
            width,
            height,
            aligned_width,
            aligned_height,
            src_width,
            src_height,
            src_aligned_width,
            src_aligned_height,
            rotation,
            quality,
            quality_level,
            frame_count: 0,
            idr_count: 0,
            prev_frame_num: 0,
            prev_poc: 0,
        })
    }

    /// Uploads a raw BGRX frame (`src_stride` bytes/row, as captured --
    /// untouched by any CPU color conversion), converts it to NV12 via
    /// VAAPI's own GPU VPP entrypoint, and encodes it as a standalone IDR
    /// frame. Returns the raw Annex-B H.264 bytes plus whether this frame was
    /// an IDR.
    pub fn encode_frame(&mut self, bgrx: &[u8], src_stride: usize) -> VaResult<EncodedFrame> {
        self.upload_bgrx_surface(bgrx, src_stride)?;
        let src = self.src_surface;
        self.encode_from_surface(src)
    }

    /// Zero-copy counterpart of `encode_frame`: takes KWin's own GPU buffer by
    /// dmabuf fd instead of a CPU-side copy of it.
    ///
    /// The shm path costs two full-frame trips across the CPU/GPU boundary per
    /// frame -- KWin does a synchronous `glReadnPixels` of the whole render
    /// target to hand us mapped bytes, then `upload_bgrx_surface` memcpys those
    /// 16.4MB (2560x1600x4) straight back onto the same GPU. Neither is needed:
    /// the pixels start and end on the iGPU, and VAAPI can import the
    /// compositor's buffer directly.
    pub fn encode_frame_dmabuf(&mut self, plane: &DmabufPlane) -> VaResult<EncodedFrame> {
        let src = self.import_dmabuf(plane)?;
        self.encode_from_surface(src)
    }

    /// Imports a dmabuf fd as a BGRX VA surface, or returns the surface already
    /// imported for that fd. `VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2` is the
    /// modifier-aware import path (the older `..._DRM_PRIME` can't express one),
    /// which matters because KWin negotiated an explicit modifier rather than
    /// leaving it implicit.
    fn import_dmabuf(&mut self, plane: &DmabufPlane) -> VaResult<ffi::VASurfaceID> {
        if let Some(&existing) = self.imported_surfaces.get(&plane.fd) {
            return Ok(existing);
        }

        let mut desc: ffi::VADRMPRIMESurfaceDescriptor = unsafe { std::mem::zeroed() };
        desc.fourcc = ffi::VA_FOURCC_BGRX;
        // Capture geometry, not output: this describes the buffer PipeWire
        // handed us. At a quarter turn the two are transposed, and using the
        // output shape here reads every row at the wrong stride -- which looks
        // like the picture sheared and drawn twice side by side.
        desc.width = self.src_width;
        desc.height = self.src_height;
        desc.num_objects = 1;
        desc.objects[0].fd = plane.fd;
        desc.objects[0].size = plane.size;
        desc.objects[0].drm_format_modifier = plane.modifier;
        desc.num_layers = 1;
        desc.layers[0].drm_format = DRM_FORMAT_XRGB8888;
        desc.layers[0].num_planes = 1;
        desc.layers[0].object_index[0] = 0;
        desc.layers[0].offset[0] = plane.offset;
        desc.layers[0].pitch[0] = plane.stride;

        let mut attribs = [
            ffi::VASurfaceAttrib {
                type_: ffi::VASurfaceAttribType_VASurfaceAttribMemoryType,
                flags: ffi::VA_SURFACE_ATTRIB_SETTABLE,
                value: ffi::VAGenericValue {
                    type_: ffi::VAGenericValueType_VAGenericValueTypeInteger,
                    value: ffi::_VAGenericValue__bindgen_ty_1 {
                        i: ffi::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2 as i32,
                    },
                },
            },
            ffi::VASurfaceAttrib {
                type_: ffi::VASurfaceAttribType_VASurfaceAttribExternalBufferDescriptor,
                flags: ffi::VA_SURFACE_ATTRIB_SETTABLE,
                value: ffi::VAGenericValue {
                    type_: ffi::VAGenericValueType_VAGenericValueTypePointer,
                    value: ffi::_VAGenericValue__bindgen_ty_1 {
                        p: &mut desc as *mut _ as *mut c_void,
                    },
                },
            },
        ];

        let mut surface: ffi::VASurfaceID = 0;
        check(
            unsafe {
                ffi::vaCreateSurfaces(
                    self.dpy,
                    ffi::VA_RT_FORMAT_RGB32,
                    self.src_width,
                    self.src_height,
                    &mut surface,
                    1,
                    attribs.as_mut_ptr(),
                    attribs.len() as u32,
                )
            },
            "vaCreateSurfaces(DRM_PRIME_2 import)",
        )?;

        eprintln!(
            "[vaapi] imported dmabuf fd {} as surface {surface} (modifier 0x{:016x}, stride {}, offset {})",
            plane.fd, plane.modifier, plane.stride, plane.offset
        );
        self.imported_surfaces.insert(plane.fd, surface);
        Ok(surface)
    }

    /// Diagnostic only: maps an imported dmabuf so the caller can read raw
    /// pixels out of it (the latency-barcode probe, specifically).
    ///
    /// The barcode instrument in `portal_capture.rs` reads the CPU mapping
    /// PipeWire hands over, which the zero-copy path deliberately no longer
    /// has -- so without this, turning on DMA-BUF would silently blind the one
    /// measurement that says whether DMA-BUF helped. Gated by the caller behind
    /// an env var: `vaDeriveImage`/`vaMapBuffer` on a GPU surface forces a sync
    /// and is exactly the kind of per-frame cost this path exists to remove.
    pub fn with_mapped_dmabuf<R>(
        &mut self,
        plane: &DmabufPlane,
        f: impl FnOnce(&[u8], usize) -> R,
    ) -> VaResult<R> {
        let surface = self.import_dmabuf(plane)?;
        let mut image: ffi::VAImage = unsafe { std::mem::zeroed() };
        check(
            unsafe { ffi::vaDeriveImage(self.dpy, surface, &mut image) },
            "vaDeriveImage(dmabuf probe)",
        )?;
        let mut buf_ptr: *mut c_void = ptr::null_mut();
        check(
            unsafe { ffi::vaMapBuffer(self.dpy, image.buf, &mut buf_ptr) },
            "vaMapBuffer(dmabuf probe)",
        )?;
        let stride = image.pitches[0] as usize;
        // Same check as `upload_bgrx_surface`, and it matters more here: this
        // image derives from a dmabuf whose stride/offset came from PipeWire's
        // buffer metadata, i.e. from the compositor, not from the VA driver
        // alone.
        // Bounded against the *capture* height: this maps a frame that arrived
        // from PipeWire, and at a quarter turn the encoder's own output is the
        // transpose of it. Using the output height here would bound the check
        // against the wrong number of rows.
        let extent = plane0_extent(&image, self.src_height, "vaDeriveImage(dmabuf probe)");
        let (offset, len) = match extent {
            Ok(v) => v,
            Err(e) => {
                unsafe { ffi::vaUnmapBuffer(self.dpy, image.buf) };
                unsafe { ffi::vaDestroyImage(self.dpy, image.image_id) };
                return Err(e);
            }
        };
        let result = unsafe {
            let base = (buf_ptr as *const u8).add(offset);
            f(std::slice::from_raw_parts(base, len), stride)
        };
        check(unsafe { ffi::vaUnmapBuffer(self.dpy, image.buf) }, "vaUnmapBuffer(dmabuf probe)")?;
        check(
            unsafe { ffi::vaDestroyImage(self.dpy, image.image_id) },
            "vaDestroyImage(dmabuf probe)",
        )?;
        Ok(result)
    }

    fn encode_from_surface(&mut self, source: ffi::VASurfaceID) -> VaResult<EncodedFrame> {
        let is_idr = self.frame_count % GOP_SIZE == 0;
        let cur_idx = (self.frame_count % 2) as usize;
        let cur_surface = self.surfaces[cur_idx];
        // The other ping-pong slot: for a P slice this still holds the
        // previous frame's reconstructed picture, untouched since we wrote
        // it two calls ago (max_num_ref_frames=1 never looks further back).
        let ref_surface = self.surfaces[1 - cur_idx];

        self.run_vpp_conversion(source, cur_surface)?;

        let mbs_w = self.aligned_width / 16;
        let mbs_h = self.aligned_height / 16;

        let coded_buf_size = self.coded_buf_size();
        let mut coded_buf: ffi::VABufferID = 0;
        check(
            unsafe {
                ffi::vaCreateBuffer(
                    self.dpy,
                    self.context_id,
                    ffi::VABufferType_VAEncCodedBufferType,
                    coded_buf_size,
                    1,
                    ptr::null_mut(),
                    &mut coded_buf,
                )
            },
            "vaCreateBuffer(coded)",
        )?;

        let crop_bottom = (self.aligned_height - self.height) / 2;
        let crop_right = (self.aligned_width - self.width) / 2;

        // Same frame_num/POC wrap points every frame regardless of IDR/P --
        // these are sequence-wide constants, must match what the last IDR's
        // packed SPS declared.
        const LOG2_MAX_FRAME_NUM_MINUS4: u32 = 4; // frame_num wraps at 256
        const LOG2_MAX_POC_LSB_MINUS4: u32 = 5; // poc_lsb wraps at 512

        let frame_num = (self.frame_count % GOP_SIZE) as u16; // 0 at each IDR
        let poc_lsb = frame_num.wrapping_mul(2);

        let mut seq: ffi::VAEncSequenceParameterBufferH264 = Default::default();
        seq.seq_parameter_set_id = 0;
        seq.level_idc = 41;
        seq.intra_period = GOP_SIZE as u32;
        seq.intra_idr_period = GOP_SIZE as u32;
        seq.ip_period = 1; // no B-frames: every non-I frame is a P
        seq.bits_per_second = self.quality.bits_per_second();
        seq.max_num_ref_frames = 1;
        seq.picture_width_in_mbs = mbs_w as u16;
        seq.picture_height_in_mbs = mbs_h as u16;
        seq.seq_fields.bits = Default::default();
        unsafe {
            seq.seq_fields.bits.set_chroma_format_idc(1);
            seq.seq_fields.bits.set_frame_mbs_only_flag(1);
            seq.seq_fields.bits.set_direct_8x8_inference_flag(1);
            seq.seq_fields.bits.set_log2_max_frame_num_minus4(LOG2_MAX_FRAME_NUM_MINUS4);
            seq.seq_fields.bits.set_pic_order_cnt_type(0);
            seq.seq_fields.bits.set_log2_max_pic_order_cnt_lsb_minus4(LOG2_MAX_POC_LSB_MINUS4);
        }
        seq.frame_cropping_flag = if crop_bottom > 0 || crop_right > 0 { 1 } else { 0 };
        seq.frame_crop_right_offset = crop_right;
        seq.frame_crop_bottom_offset = crop_bottom;

        let mut pic: ffi::VAEncPictureParameterBufferH264 = Default::default();
        pic.CurrPic.picture_id = cur_surface;
        pic.CurrPic.frame_idx = frame_num as u32;
        pic.CurrPic.flags = 0;
        pic.CurrPic.TopFieldOrderCnt = poc_lsb as i32;
        pic.CurrPic.BottomFieldOrderCnt = poc_lsb as i32;
        for r in pic.ReferenceFrames.iter_mut() {
            r.picture_id = ffi::VA_INVALID_SURFACE;
            r.flags = ffi::VA_PICTURE_H264_INVALID;
        }
        if !is_idr {
            // Sole reference for the P slice: the previous frame, still
            // sitting in the other ping-pong slot.
            pic.ReferenceFrames[0].picture_id = ref_surface;
            pic.ReferenceFrames[0].frame_idx = self.prev_frame_num as u32;
            pic.ReferenceFrames[0].flags = ffi::VA_PICTURE_H264_SHORT_TERM_REFERENCE;
            pic.ReferenceFrames[0].TopFieldOrderCnt = self.prev_poc;
            pic.ReferenceFrames[0].BottomFieldOrderCnt = self.prev_poc;
        }
        pic.coded_buf = coded_buf;
        pic.pic_parameter_set_id = 0;
        pic.seq_parameter_set_id = 0;
        pic.last_picture = 0;
        pic.frame_num = frame_num;
        pic.pic_init_qp = 26;
        pic.num_ref_idx_l0_active_minus1 = 0;
        pic.num_ref_idx_l1_active_minus1 = 0;
        pic.chroma_qp_index_offset = 0;
        pic.second_chroma_qp_index_offset = 0;
        pic.pic_fields.bits = Default::default();
        unsafe {
            pic.pic_fields.bits.set_idr_pic_flag(is_idr as u32);
            // Both I(DR) and P frames here are referenced by the next P in
            // the IPPP chain, so both are reference pictures.
            pic.pic_fields.bits.set_reference_pic_flag(1);
            // CABAC + Main profile instead of CAVLC + Constrained Baseline: the
            // tablet's hardware decoder rendered solid green with CAVLC/Baseline
            // (confirmed decoding, just wrong colors) while both ffmpeg and
            // Android's software decoder handled the same bytes fine -- CABAC is
            // the far more heavily used/tested path on most decoder silicon.
            pic.pic_fields.bits.set_entropy_coding_mode_flag(1);
            pic.pic_fields.bits.set_deblocking_filter_control_present_flag(1);
        }

        let mut slice: ffi::VAEncSliceParameterBufferH264 = Default::default();
        slice.macroblock_address = 0;
        slice.num_macroblocks = mbs_w * mbs_h;
        slice.macroblock_info = ffi::VA_INVALID_ID;
        slice.slice_type = if is_idr { 2 } else { 0 }; // I slice : P slice
        slice.pic_parameter_set_id = 0;
        slice.idr_pic_id = self.idr_count;
        slice.pic_order_cnt_lsb = poc_lsb;
        slice.direct_spatial_mv_pred_flag = 0;
        slice.num_ref_idx_active_override_flag = 0;
        slice.slice_qp_delta = 0;
        slice.disable_deblocking_filter_idc = 0;
        for r in slice.RefPicList0.iter_mut() {
            r.picture_id = ffi::VA_INVALID_SURFACE;
            r.flags = ffi::VA_PICTURE_H264_INVALID;
        }
        if !is_idr {
            slice.RefPicList0[0] = pic.ReferenceFrames[0];
        }

        // The iHD driver's low-power H.264 entrypoint only emits slice data
        // itself -- it does not synthesize SPS/PPS. Build them by hand and
        // hand them over as "packed header" buffers, only on IDR frames --
        // P slices in between rely on the parameter sets already active on
        // the decoder from the last IDR, standard Annex-B convention.
        let mut packed_seq_param_buf: ffi::VABufferID = 0;
        let mut packed_seq_data_buf: ffi::VABufferID = 0;
        let mut packed_pic_param_buf: ffi::VABufferID = 0;
        let mut packed_pic_data_buf: ffi::VABufferID = 0;
        if is_idr {
            let h264_params = H264Params {
                profile_idc: 77, // Main
                level_idc: seq.level_idc,
                mbs_width: mbs_w,
                mbs_height: mbs_h,
                max_num_ref_frames: seq.max_num_ref_frames,
                log2_max_frame_num_minus4: LOG2_MAX_FRAME_NUM_MINUS4,
                log2_max_pic_order_cnt_lsb_minus4: LOG2_MAX_POC_LSB_MINUS4,
                frame_crop_right: seq.frame_crop_right_offset,
                frame_crop_bottom: seq.frame_crop_bottom_offset,
                pic_init_qp: pic.pic_init_qp,
                deblocking_filter_control_present: true,
                // Smallest value the spec allows here: it must be at least
                // `max_num_ref_frames`, and every frame above it is a frame the
                // decoder is entitled to buffer before emitting output. Tied to
                // the same field rather than written as its own constant so the
                // two can't drift apart -- moonlight-android carries an explicit
                // "some devices throw errors if maxDecFrameBuffering <
                // numRefFrames" note for exactly this.
                max_dec_frame_buffering: seq.max_num_ref_frames,
            };
            let sps_bytes = h264_headers::build_sps(&h264_params);
            let pps_bytes = h264_headers::build_pps(&h264_params);
            self.create_packed_header(
                ffi::VAEncPackedHeaderType_VAEncPackedHeaderSequence,
                &sps_bytes,
                &mut packed_seq_param_buf,
                &mut packed_seq_data_buf,
            )?;
            self.create_packed_header(
                ffi::VAEncPackedHeaderType_VAEncPackedHeaderPicture,
                &pps_bytes,
                &mut packed_pic_param_buf,
                &mut packed_pic_data_buf,
            )?;
        }

        let mut seq_buf: ffi::VABufferID = 0;
        let mut pic_buf: ffi::VABufferID = 0;
        let mut slice_buf: ffi::VABufferID = 0;
        check(
            unsafe {
                ffi::vaCreateBuffer(
                    self.dpy,
                    self.context_id,
                    ffi::VABufferType_VAEncSequenceParameterBufferType,
                    std::mem::size_of::<ffi::VAEncSequenceParameterBufferH264>() as u32,
                    1,
                    &mut seq as *mut _ as *mut c_void,
                    &mut seq_buf,
                )
            },
            "vaCreateBuffer(seq)",
        )?;
        check(
            unsafe {
                ffi::vaCreateBuffer(
                    self.dpy,
                    self.context_id,
                    ffi::VABufferType_VAEncPictureParameterBufferType,
                    std::mem::size_of::<ffi::VAEncPictureParameterBufferH264>() as u32,
                    1,
                    &mut pic as *mut _ as *mut c_void,
                    &mut pic_buf,
                )
            },
            "vaCreateBuffer(pic)",
        )?;
        check(
            unsafe {
                ffi::vaCreateBuffer(
                    self.dpy,
                    self.context_id,
                    ffi::VABufferType_VAEncSliceParameterBufferType,
                    std::mem::size_of::<ffi::VAEncSliceParameterBufferH264>() as u32,
                    1,
                    &mut slice as *mut _ as *mut c_void,
                    &mut slice_buf,
                )
            },
            "vaCreateBuffer(slice)",
        )?;

        check(
            unsafe { ffi::vaBeginPicture(self.dpy, self.context_id, cur_surface) },
            "vaBeginPicture",
        )?;
        let mut named_buffers: Vec<(&str, ffi::VABufferID)> = Vec::with_capacity(7);
        if is_idr {
            named_buffers.push(("packed_seq_param", packed_seq_param_buf));
            named_buffers.push(("packed_seq_data", packed_seq_data_buf));
        }
        named_buffers.push(("seq", seq_buf));
        if is_idr {
            named_buffers.push(("packed_pic_param", packed_pic_param_buf));
            named_buffers.push(("packed_pic_data", packed_pic_data_buf));
        }
        named_buffers.push(("pic", pic_buf));
        // Submitted alongside the picture, not at config time: VAAPI carries
        // encoder speed/quality as a per-picture "misc parameter" buffer rather
        // than a config attribute.
        let mut quality_buf: ffi::VABufferID = 0;
        if self.quality_level > 0 {
            self.create_quality_level_buffer(&mut quality_buf)?;
            named_buffers.push(("quality_level", quality_buf));
        }
        named_buffers.push(("slice", slice_buf));
        for (name, mut id) in named_buffers {
            let status = unsafe { ffi::vaRenderPicture(self.dpy, self.context_id, &mut id, 1) };
            if status as u32 != ffi::VA_STATUS_SUCCESS {
                return Err(format!("vaRenderPicture({name}) failed: VAStatus={status}"));
            }
        }
        check(
            unsafe { ffi::vaEndPicture(self.dpy, self.context_id) },
            "vaEndPicture",
        )?;
        check(
            unsafe { ffi::vaSyncSurface(self.dpy, cur_surface) },
            "vaSyncSurface",
        )?;

        let out = self.read_coded_buffer(coded_buf)?;

        check(
            unsafe { ffi::vaDestroyBuffer(self.dpy, coded_buf) },
            "vaDestroyBuffer(coded)",
        )?;

        self.prev_frame_num = frame_num;
        self.prev_poc = poc_lsb as i32;
        if is_idr {
            self.idr_count = self.idr_count.wrapping_add(1);
        }
        self.frame_count += 1;

        Ok(EncodedFrame { data: out, is_idr })
    }

    /// A `VAEncMiscParameterBuffer` is a variable-length header (`type`)
    /// followed inline by the type-specific payload, so it has to be allocated
    /// at the combined size and filled through a mapping rather than passed as
    /// a plain struct like the seq/pic/slice buffers.
    fn create_quality_level_buffer(&self, buf: &mut ffi::VABufferID) -> VaResult<()> {
        let size = (std::mem::size_of::<ffi::VAEncMiscParameterBuffer>()
            + std::mem::size_of::<ffi::VAEncMiscParameterBufferQualityLevel>())
            as u32;
        check(
            unsafe {
                ffi::vaCreateBuffer(
                    self.dpy,
                    self.context_id,
                    ffi::VABufferType_VAEncMiscParameterBufferType,
                    size,
                    1,
                    ptr::null_mut(),
                    buf,
                )
            },
            "vaCreateBuffer(quality level)",
        )?;

        let mut ptr_out: *mut c_void = ptr::null_mut();
        check(
            unsafe { ffi::vaMapBuffer(self.dpy, *buf, &mut ptr_out) },
            "vaMapBuffer(quality level)",
        )?;
        unsafe {
            let misc = ptr_out as *mut ffi::VAEncMiscParameterBuffer;
            (*misc).type_ = ffi::VAEncMiscParameterType_VAEncMiscParameterTypeQualityLevel;
            let payload = (*misc).data.as_mut_ptr() as *mut ffi::VAEncMiscParameterBufferQualityLevel;
            (*payload).quality_level = self.quality_level;
        }
        check(
            unsafe { ffi::vaUnmapBuffer(self.dpy, *buf) },
            "vaUnmapBuffer(quality level)",
        )?;
        Ok(())
    }

    fn create_packed_header(
        &self,
        header_type: u32,
        nal_bytes: &[u8],
        param_buf: &mut ffi::VABufferID,
        data_buf: &mut ffi::VABufferID,
    ) -> VaResult<()> {
        let mut param = ffi::VAEncPackedHeaderParameterBuffer {
            type_: header_type,
            bit_length: (nal_bytes.len() * 8) as u32,
            has_emulation_bytes: 1, // we already inserted them, see h264_headers.rs
            va_reserved: [0; 4],
        };
        check(
            unsafe {
                ffi::vaCreateBuffer(
                    self.dpy,
                    self.context_id,
                    ffi::VABufferType_VAEncPackedHeaderParameterBufferType,
                    std::mem::size_of::<ffi::VAEncPackedHeaderParameterBuffer>() as u32,
                    1,
                    &mut param as *mut _ as *mut c_void,
                    param_buf,
                )
            },
            "vaCreateBuffer(packed header param)",
        )?;

        let mut data = nal_bytes.to_vec();
        check(
            unsafe {
                ffi::vaCreateBuffer(
                    self.dpy,
                    self.context_id,
                    ffi::VABufferType_VAEncPackedHeaderDataBufferType,
                    data.len() as u32,
                    1,
                    data.as_mut_ptr() as *mut c_void,
                    data_buf,
                )
            },
            "vaCreateBuffer(packed header data)",
        )?;
        Ok(())
    }

    /// Straight memcpy of raw BGRX rows into the source surface -- no
    /// per-pixel math, that's VPP's job now (see `run_vpp_conversion`).
    fn upload_bgrx_surface(&mut self, bgrx: &[u8], src_stride: usize) -> VaResult<()> {
        let mut image: ffi::VAImage = unsafe { std::mem::zeroed() };
        check(
            unsafe { ffi::vaDeriveImage(self.dpy, self.src_surface, &mut image) },
            "vaDeriveImage(src)",
        )?;

        let mut buf_ptr: *mut c_void = ptr::null_mut();
        check(
            unsafe { ffi::vaMapBuffer(self.dpy, image.buf, &mut buf_ptr) },
            "vaMapBuffer(src)",
        )?;

        // Unmap and destroy before propagating: this is the one write into
        // driver-mapped memory in the whole encoder, so it gets checked, and a
        // rejected frame shouldn't also leak the mapping it declined to use.
        // The BGRX surface is capture-shaped, so it is the source alignment that
        // bounds it, not the encoder's output alignment.
        let extent = plane0_extent(&image, self.src_aligned_height, "vaDeriveImage(src)");
        let (offset, len) = match extent {
            Ok(v) => v,
            Err(e) => {
                unsafe { ffi::vaUnmapBuffer(self.dpy, image.buf) };
                unsafe { ffi::vaDestroyImage(self.dpy, image.image_id) };
                return Err(e);
            }
        };

        unsafe {
            let base = buf_ptr as *mut u8;
            let dst = std::slice::from_raw_parts_mut(base.add(offset), len);
            let row_bytes = self.src_width as usize * 4;
            for row in 0..self.src_height as usize {
                let src = &bgrx[row * src_stride..][..row_bytes];
                let d = &mut dst[row * image.pitches[0] as usize..][..row_bytes];
                d.copy_from_slice(src);
            }
        }

        check(unsafe { ffi::vaUnmapBuffer(self.dpy, image.buf) }, "vaUnmapBuffer(src)")?;
        check(
            unsafe { ffi::vaDestroyImage(self.dpy, image.image_id) },
            "vaDestroyImage(src)",
        )?;
        Ok(())
    }

    /// Converts `source` (BGRX -- either the CPU-uploaded staging surface or an
    /// imported PipeWire dmabuf) into `target_surface` (NV12) entirely on the
    /// GPU via VAAPI's Video Post-Processing entrypoint.
    fn run_vpp_conversion(
        &mut self,
        source: ffi::VASurfaceID,
        target_surface: ffi::VASurfaceID,
    ) -> VaResult<()> {
        check(
            unsafe { ffi::vaBeginPicture(self.dpy, self.vpp_context_id, target_surface) },
            "vaBeginPicture(VPP)",
        )?;

        let mut pipeline_param: ffi::VAProcPipelineParameterBuffer = Default::default();
        pipeline_param.surface = source;
        pipeline_param.rotation_state = match self.rotation {
            crate::protocol::Rotation::None => ffi::VA_ROTATION_NONE,
            crate::protocol::Rotation::Quarter => ffi::VA_ROTATION_90,
            crate::protocol::Rotation::Half => ffi::VA_ROTATION_180,
            crate::protocol::Rotation::ThreeQuarters => ffi::VA_ROTATION_270,
        };

        let mut pipeline_buf: ffi::VABufferID = 0;
        check(
            unsafe {
                ffi::vaCreateBuffer(
                    self.dpy,
                    self.vpp_context_id,
                    ffi::VABufferType_VAProcPipelineParameterBufferType,
                    std::mem::size_of::<ffi::VAProcPipelineParameterBuffer>() as u32,
                    1,
                    &mut pipeline_param as *mut _ as *mut c_void,
                    &mut pipeline_buf,
                )
            },
            "vaCreateBuffer(VPP pipeline)",
        )?;
        check(
            unsafe { ffi::vaRenderPicture(self.dpy, self.vpp_context_id, &mut pipeline_buf, 1) },
            "vaRenderPicture(VPP)",
        )?;
        check(
            unsafe { ffi::vaEndPicture(self.dpy, self.vpp_context_id) },
            "vaEndPicture(VPP)",
        )?;
        // Deliberately no vaSyncSurface here. The encode that follows targets
        // this same surface and ends with its own sync, and VAAPI orders
        // operations on a surface for us -- syncing in between only parks the
        // CPU between two GPU jobs that could otherwise overlap. Verified by
        // encoding the same 150 synthetic frames with and without it and
        // diffing the output: byte-identical, so the ordering guarantee holds
        // in practice on iHD, not just on paper.
        check(
            unsafe { ffi::vaDestroyBuffer(self.dpy, pipeline_buf) },
            "vaDestroyBuffer(VPP pipeline)",
        )?;
        Ok(())
    }

    /// Size requested for the coded (bitstream output) buffer. A method rather
    /// than a local so `read_coded_buffer` can bound what the driver reports
    /// against the very number the allocation asked for.
    fn coded_buf_size(&self) -> u32 {
        (self.aligned_width * self.aligned_height * 3 / 2) + 0x10000
    }

    fn read_coded_buffer(&self, coded_buf: ffi::VABufferID) -> VaResult<Vec<u8>> {
        let mut buf_ptr: *mut c_void = ptr::null_mut();
        check(
            unsafe { ffi::vaMapBuffer(self.dpy, coded_buf, &mut buf_ptr) },
            "vaMapBuffer(coded)",
        )?;

        // Collect first, unmap unconditionally, then propagate -- a rejected
        // frame must not leave the coded buffer mapped.
        let collected = unsafe { collect_coded_segments(buf_ptr, self.coded_buf_size() as usize) };
        check(unsafe { ffi::vaUnmapBuffer(self.dpy, coded_buf) }, "vaUnmapBuffer(coded)")?;
        collected
    }
}

impl Drop for VaapiEncoder {
    fn drop(&mut self) {
        unsafe {
            ffi::vaDestroyContext(self.dpy, self.vpp_context_id);
            ffi::vaDestroyConfig(self.dpy, self.vpp_config_id);
            for (_, mut imported) in self.imported_surfaces.drain() {
                ffi::vaDestroySurfaces(self.dpy, &mut imported, 1);
            }
            let mut src = self.src_surface;
            ffi::vaDestroySurfaces(self.dpy, &mut src, 1);
            ffi::vaDestroyContext(self.dpy, self.context_id);
            ffi::vaDestroyConfig(self.dpy, self.config_id);
            let mut s = self.surfaces;
            ffi::vaDestroySurfaces(self.dpy, s.as_mut_ptr(), s.len() as i32);
            ffi::vaTerminate(self.dpy);
            libc::close(self.render_fd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(offset: u32, pitch: u32, data_size: u32) -> ffi::VAImage {
        let mut img: ffi::VAImage = unsafe { std::mem::zeroed() };
        img.offsets[0] = offset;
        img.pitches[0] = pitch;
        img.data_size = data_size;
        img
    }

    #[test]
    fn plane_extent_accepts_a_buffer_that_actually_holds_the_plane() {
        // 100 rows of 256 bytes starting at 0, in a buffer that says it has room.
        let (offset, len) = plane0_extent(&image(0, 256, 25_600), 100, "t").unwrap();
        assert_eq!((offset, len), (0, 25_600));
    }

    #[test]
    fn plane_extent_rejects_a_plane_running_past_the_mapping() {
        // One byte short: the old code would have built a slice over it anyway.
        let err = plane0_extent(&image(0, 256, 25_599), 100, "t").unwrap_err();
        assert!(err.contains("25600"), "{err}");
        assert!(err.contains("25599"), "{err}");
    }

    #[test]
    fn plane_extent_rejects_an_offset_that_pushes_the_plane_out_of_range() {
        // Plane itself fits, but not where the driver says it starts.
        assert!(plane0_extent(&image(1_024, 256, 25_600), 100, "t").is_err());
    }

    #[test]
    fn plane_extent_rejects_arithmetic_overflow() {
        assert!(plane0_extent(&image(0, u32::MAX, u32::MAX), 4, "t").is_err());
        assert!(plane0_extent(&image(u32::MAX, 16, u32::MAX), 4, "t").is_err());
    }

    fn segment(data: &mut [u8], size: u32, next: *mut c_void) -> ffi::VACodedBufferSegment {
        let mut s: ffi::VACodedBufferSegment = unsafe { std::mem::zeroed() };
        s.size = size;
        s.buf = data.as_mut_ptr() as *mut c_void;
        s.next = next;
        s
    }

    #[test]
    fn collects_a_well_formed_segment_chain_in_order() {
        let (mut a, mut b) = (vec![1u8; 3], vec![2u8; 2]);
        let mut second = segment(&mut b, 2, ptr::null_mut());
        let p2: *mut ffi::VACodedBufferSegment = &mut second;
        let mut first = segment(&mut a, 3, p2 as *mut c_void);
        let p1: *mut ffi::VACodedBufferSegment = &mut first;

        let out = unsafe { collect_coded_segments(p1 as *mut c_void, 1024) }.unwrap();
        assert_eq!(out, vec![1, 1, 1, 2, 2]);
    }

    #[test]
    fn rejects_a_segment_claiming_more_bytes_than_were_allocated() {
        // The disclosure case: an inflated `size` used to copy whatever followed
        // the real buffer into the frame sent to the phone.
        let mut data = vec![7u8; 8];
        let mut s = segment(&mut data, 500_000, ptr::null_mut());
        let p: *mut ffi::VACodedBufferSegment = &mut s;

        let err = unsafe { collect_coded_segments(p as *mut c_void, 1024) }.unwrap_err();
        assert!(err.contains("500000"), "{err}");
    }

    #[test]
    fn rejects_a_cyclic_segment_list_instead_of_looping_forever() {
        let mut data = vec![9u8; 4];
        let mut s = segment(&mut data, 4, ptr::null_mut());
        let p: *mut ffi::VACodedBufferSegment = &mut s;
        unsafe { (*p).next = p as *mut c_void };

        let err = unsafe { collect_coded_segments(p as *mut c_void, 1_048_576) }.unwrap_err();
        assert!(err.contains("still going"), "{err}");
    }

    #[test]
    fn rejects_a_segment_with_a_null_data_pointer() {
        let mut s: ffi::VACodedBufferSegment = unsafe { std::mem::zeroed() };
        s.size = 4;
        let p: *mut ffi::VACodedBufferSegment = &mut s;

        let err = unsafe { collect_coded_segments(p as *mut c_void, 1024) }.unwrap_err();
        assert!(err.contains("null data pointer"), "{err}");
    }

    #[test]
    fn accumulates_toward_the_cap_across_segments() {
        // Each segment fits on its own; together they overrun. The old code had
        // no notion of a running total at all.
        let (mut a, mut b) = (vec![0u8; 600], vec![0u8; 600]);
        let mut second = segment(&mut b, 600, ptr::null_mut());
        let p2: *mut ffi::VACodedBufferSegment = &mut second;
        let mut first = segment(&mut a, 600, p2 as *mut c_void);
        let p1: *mut ffi::VACodedBufferSegment = &mut first;

        assert!(unsafe { collect_coded_segments(p1 as *mut c_void, 1024) }.is_err());
    }
}
