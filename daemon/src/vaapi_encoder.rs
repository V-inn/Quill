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
    vpp_config_id: ffi::VAConfigID,
    vpp_context_id: ffi::VAContextID,
    width: u32,
    height: u32,
    aligned_width: u32,
    aligned_height: u32,
    // Milestone 16: KWin's rotation property has no effect on what a
    // krfb-virtualmonitor output's screencast producer actually exports --
    // confirmed live, toggling it (even via System Settings directly, not
    // just our own automation) changed kscreen-doctor's reported metadata
    // but never the captured pixels. VPP's own rotation_state is the
    // GPU-accelerated place that does work, applied here instead.
    flip_180: bool,
    // GOP state: total frames encoded so far (drives the ping-pong slot and
    // the IDR/P decision), a counter distinguishing successive IDRs
    // (idr_pic_id must differ between them even though frame_num resets to
    // 0 each time), and the previous frame's frame_num/POC -- needed to
    // populate the P slice's single reference-picture entry.
    frame_count: u64,
    idr_count: u16,
    prev_frame_num: u16,
    prev_poc: i32,
}

impl VaapiEncoder {
    pub fn new(width: u32, height: u32, flip_180: bool) -> VaResult<Self> {
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
                    aligned_width,
                    aligned_height,
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

        eprintln!(
            "[vaapi] encoder ready: {width}x{height} (aligned {aligned_width}x{aligned_height}), GPU color conversion via VPP, GOP {GOP_SIZE}"
        );

        Ok(Self {
            dpy,
            render_fd,
            config_id,
            context_id,
            surfaces,
            src_surface,
            vpp_config_id,
            vpp_context_id,
            width,
            height,
            aligned_width,
            aligned_height,
            flip_180,
            frame_count: 0,
            idr_count: 0,
            prev_frame_num: 0,
            prev_poc: 0,
        })
    }

    /// Uploads a raw BGRX frame (`src_stride` bytes/row, as captured --
    /// untouched by any CPU color conversion), converts it to NV12 via
    /// VAAPI's own GPU VPP entrypoint, and encodes it as a standalone IDR
    /// frame. Returns the raw Annex-B H.264 bytes.
    pub fn encode_frame(&mut self, bgrx: &[u8], src_stride: usize) -> VaResult<Vec<u8>> {
        let is_idr = self.frame_count % GOP_SIZE == 0;
        let cur_idx = (self.frame_count % 2) as usize;
        let cur_surface = self.surfaces[cur_idx];
        // The other ping-pong slot: for a P slice this still holds the
        // previous frame's reconstructed picture, untouched since we wrote
        // it two calls ago (max_num_ref_frames=1 never looks further back).
        let ref_surface = self.surfaces[1 - cur_idx];

        self.upload_bgrx_surface(bgrx, src_stride)?;
        self.run_vpp_conversion(cur_surface)?;

        let mbs_w = self.aligned_width / 16;
        let mbs_h = self.aligned_height / 16;

        let coded_buf_size = (self.aligned_width * self.aligned_height * 3 / 2) + 0x10000;
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
        seq.bits_per_second = 20_000_000;
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

        Ok(out)
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

        unsafe {
            let base = buf_ptr as *mut u8;
            let dst = std::slice::from_raw_parts_mut(
                base.add(image.offsets[0] as usize),
                (image.pitches[0] as usize) * (self.aligned_height as usize),
            );
            let row_bytes = self.width as usize * 4;
            for row in 0..self.height as usize {
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

    /// Converts `src_surface` (BGRX) into `surface` (NV12) entirely on the
    /// GPU via VAAPI's Video Post-Processing entrypoint.
    fn run_vpp_conversion(&mut self, target_surface: ffi::VASurfaceID) -> VaResult<()> {
        check(
            unsafe { ffi::vaBeginPicture(self.dpy, self.vpp_context_id, target_surface) },
            "vaBeginPicture(VPP)",
        )?;

        let mut pipeline_param: ffi::VAProcPipelineParameterBuffer = Default::default();
        pipeline_param.surface = self.src_surface;
        pipeline_param.rotation_state = if self.flip_180 { ffi::VA_ROTATION_180 } else { ffi::VA_ROTATION_NONE };

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
        check(
            unsafe { ffi::vaSyncSurface(self.dpy, target_surface) },
            "vaSyncSurface(VPP)",
        )?;
        check(
            unsafe { ffi::vaDestroyBuffer(self.dpy, pipeline_buf) },
            "vaDestroyBuffer(VPP pipeline)",
        )?;
        Ok(())
    }

    fn read_coded_buffer(&self, coded_buf: ffi::VABufferID) -> VaResult<Vec<u8>> {
        let mut buf_ptr: *mut c_void = ptr::null_mut();
        check(
            unsafe { ffi::vaMapBuffer(self.dpy, coded_buf, &mut buf_ptr) },
            "vaMapBuffer(coded)",
        )?;

        let mut out = Vec::new();
        unsafe {
            let mut seg = buf_ptr as *const ffi::VACodedBufferSegment;
            while !seg.is_null() {
                let s = &*seg;
                let bytes = std::slice::from_raw_parts(s.buf as *const u8, s.size as usize);
                out.extend_from_slice(bytes);
                seg = s.next as *const ffi::VACodedBufferSegment;
            }
        }

        check(unsafe { ffi::vaUnmapBuffer(self.dpy, coded_buf) }, "vaUnmapBuffer(coded)")?;
        Ok(out)
    }
}

impl Drop for VaapiEncoder {
    fn drop(&mut self) {
        unsafe {
            ffi::vaDestroyContext(self.dpy, self.vpp_context_id);
            ffi::vaDestroyConfig(self.dpy, self.vpp_config_id);
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
