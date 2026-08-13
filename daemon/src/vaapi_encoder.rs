//! Minimal VAAPI H.264 encoder: every frame is an independent IDR (all-intra).
//!
//! v0 scope (Milestone 2): correctness and latency measurement, not bitrate
//! efficiency -- a real GOP structure with P-frames is tuning-pass work
//! (Milestone 7). Encoding every frame independently also sidesteps
//! reference-picture-list bookkeeping entirely, which keeps this small.

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

pub struct VaapiEncoder {
    dpy: ffi::VADisplay,
    render_fd: std::os::raw::c_int,
    config_id: ffi::VAConfigID,
    context_id: ffi::VAContextID,
    surface: ffi::VASurfaceID,
    width: u32,
    height: u32,
    aligned_width: u32,
    aligned_height: u32,
}

impl VaapiEncoder {
    pub fn new(width: u32, height: u32) -> VaResult<Self> {
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
        let mut surface: ffi::VASurfaceID = 0;
        check(
            unsafe {
                ffi::vaCreateSurfaces(
                    dpy,
                    ffi::VA_RT_FORMAT_YUV420,
                    aligned_width,
                    aligned_height,
                    &mut surface,
                    1,
                    &mut pixel_format_attrib,
                    1,
                )
            },
            "vaCreateSurfaces",
        )?;

        let mut context_id: ffi::VAContextID = 0;
        let mut render_targets = [surface];
        check(
            unsafe {
                ffi::vaCreateContext(
                    dpy,
                    config_id,
                    aligned_width as i32,
                    aligned_height as i32,
                    0, // no VA_PROGRESSIVE flag needed for encode
                    render_targets.as_mut_ptr(),
                    1,
                    &mut context_id,
                )
            },
            "vaCreateContext",
        )?;

        eprintln!(
            "[vaapi] encoder ready: {width}x{height} (aligned {aligned_width}x{aligned_height})"
        );

        Ok(Self {
            dpy,
            render_fd,
            config_id,
            context_id,
            surface,
            width,
            height,
            aligned_width,
            aligned_height,
        })
    }

    /// Uploads `nv12` (already-converted Y+UV planes, tightly packed at
    /// aligned_width/aligned_height) into the surface and encodes it as a
    /// standalone IDR frame. Returns the raw Annex-B H.264 bytes.
    pub fn encode_frame(&mut self, y_plane: &[u8], uv_plane: &[u8]) -> VaResult<Vec<u8>> {
        self.upload_surface(y_plane, uv_plane)?;

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

        let mut seq: ffi::VAEncSequenceParameterBufferH264 = Default::default();
        seq.seq_parameter_set_id = 0;
        seq.level_idc = 41;
        seq.intra_period = 1;
        seq.intra_idr_period = 1;
        seq.ip_period = 1;
        seq.bits_per_second = 20_000_000;
        seq.max_num_ref_frames = 1;
        seq.picture_width_in_mbs = mbs_w as u16;
        seq.picture_height_in_mbs = mbs_h as u16;
        seq.seq_fields.bits = Default::default();
        unsafe {
            seq.seq_fields.bits.set_chroma_format_idc(1);
            seq.seq_fields.bits.set_frame_mbs_only_flag(1);
            seq.seq_fields.bits.set_direct_8x8_inference_flag(1);
            seq.seq_fields.bits.set_log2_max_frame_num_minus4(0);
            seq.seq_fields.bits.set_pic_order_cnt_type(0);
            seq.seq_fields.bits.set_log2_max_pic_order_cnt_lsb_minus4(0);
        }
        seq.frame_cropping_flag = if crop_bottom > 0 || crop_right > 0 { 1 } else { 0 };
        seq.frame_crop_right_offset = crop_right;
        seq.frame_crop_bottom_offset = crop_bottom;

        let mut pic: ffi::VAEncPictureParameterBufferH264 = Default::default();
        pic.CurrPic.picture_id = self.surface;
        pic.CurrPic.frame_idx = 0;
        pic.CurrPic.flags = 0;
        for r in pic.ReferenceFrames.iter_mut() {
            r.picture_id = ffi::VA_INVALID_SURFACE;
            r.flags = ffi::VA_PICTURE_H264_INVALID;
        }
        pic.coded_buf = coded_buf;
        pic.pic_parameter_set_id = 0;
        pic.seq_parameter_set_id = 0;
        pic.last_picture = 0;
        pic.frame_num = 0;
        pic.pic_init_qp = 26;
        pic.num_ref_idx_l0_active_minus1 = 0;
        pic.num_ref_idx_l1_active_minus1 = 0;
        pic.chroma_qp_index_offset = 0;
        pic.second_chroma_qp_index_offset = 0;
        pic.pic_fields.bits = Default::default();
        unsafe {
            pic.pic_fields.bits.set_idr_pic_flag(1);
            pic.pic_fields.bits.set_reference_pic_flag(0);
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
        slice.slice_type = 2; // I slice
        slice.pic_parameter_set_id = 0;
        slice.idr_pic_id = 0;
        slice.pic_order_cnt_lsb = 0;
        slice.direct_spatial_mv_pred_flag = 0;
        slice.num_ref_idx_active_override_flag = 0;
        slice.slice_qp_delta = 0;
        slice.disable_deblocking_filter_idc = 0;

        // The iHD driver's low-power H.264 entrypoint only emits slice data
        // itself -- it does not synthesize SPS/PPS. Build them by hand and
        // hand them over as "packed header" buffers so every frame (each an
        // independent IDR) is standalone-decodable.
        let h264_params = H264Params {
            profile_idc: 77, // Main
            level_idc: seq.level_idc,
            mbs_width: mbs_w,
            mbs_height: mbs_h,
            max_num_ref_frames: seq.max_num_ref_frames,
            log2_max_frame_num_minus4: 0,
            log2_max_pic_order_cnt_lsb_minus4: 0,
            frame_crop_right: seq.frame_crop_right_offset,
            frame_crop_bottom: seq.frame_crop_bottom_offset,
            pic_init_qp: pic.pic_init_qp,
            deblocking_filter_control_present: true,
        };
        let sps_bytes = h264_headers::build_sps(&h264_params);
        let pps_bytes = h264_headers::build_pps(&h264_params);

        let mut packed_seq_param_buf: ffi::VABufferID = 0;
        let mut packed_seq_data_buf: ffi::VABufferID = 0;
        let mut packed_pic_param_buf: ffi::VABufferID = 0;
        let mut packed_pic_data_buf: ffi::VABufferID = 0;
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
            unsafe { ffi::vaBeginPicture(self.dpy, self.context_id, self.surface) },
            "vaBeginPicture",
        )?;
        let named_buffers: [(&str, ffi::VABufferID); 7] = [
            ("packed_seq_param", packed_seq_param_buf),
            ("packed_seq_data", packed_seq_data_buf),
            ("seq", seq_buf),
            ("packed_pic_param", packed_pic_param_buf),
            ("packed_pic_data", packed_pic_data_buf),
            ("pic", pic_buf),
            ("slice", slice_buf),
        ];
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
            unsafe { ffi::vaSyncSurface(self.dpy, self.surface) },
            "vaSyncSurface",
        )?;

        let out = self.read_coded_buffer(coded_buf)?;

        check(
            unsafe { ffi::vaDestroyBuffer(self.dpy, coded_buf) },
            "vaDestroyBuffer(coded)",
        )?;

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

    fn upload_surface(&mut self, y_plane: &[u8], uv_plane: &[u8]) -> VaResult<()> {
        let mut image: ffi::VAImage = unsafe { std::mem::zeroed() };
        check(
            unsafe { ffi::vaDeriveImage(self.dpy, self.surface, &mut image) },
            "vaDeriveImage",
        )?;

        let mut buf_ptr: *mut c_void = ptr::null_mut();
        check(
            unsafe { ffi::vaMapBuffer(self.dpy, image.buf, &mut buf_ptr) },
            "vaMapBuffer",
        )?;

        unsafe {
            let base = buf_ptr as *mut u8;
            let y_dst = std::slice::from_raw_parts_mut(
                base.add(image.offsets[0] as usize),
                (image.pitches[0] as usize) * (self.aligned_height as usize),
            );
            for row in 0..self.height as usize {
                let src = &y_plane[row * self.aligned_width as usize..][..self.width as usize];
                let dst = &mut y_dst[row * image.pitches[0] as usize..][..self.width as usize];
                dst.copy_from_slice(src);
            }

            let uv_dst = std::slice::from_raw_parts_mut(
                base.add(image.offsets[1] as usize),
                (image.pitches[1] as usize) * (self.aligned_height as usize / 2),
            );
            for row in 0..(self.height as usize / 2) {
                let src = &uv_plane[row * self.aligned_width as usize..][..self.width as usize];
                let dst = &mut uv_dst[row * image.pitches[1] as usize..][..self.width as usize];
                dst.copy_from_slice(src);
            }
        }

        check(unsafe { ffi::vaUnmapBuffer(self.dpy, image.buf) }, "vaUnmapBuffer")?;
        check(
            unsafe { ffi::vaDestroyImage(self.dpy, image.image_id) },
            "vaDestroyImage",
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

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn aligned_width(&self) -> u32 {
        self.aligned_width
    }
    pub fn aligned_height(&self) -> u32 {
        self.aligned_height
    }
}

impl Drop for VaapiEncoder {
    fn drop(&mut self) {
        unsafe {
            ffi::vaDestroyContext(self.dpy, self.context_id);
            ffi::vaDestroyConfig(self.dpy, self.config_id);
            let mut s = self.surface;
            ffi::vaDestroySurfaces(self.dpy, &mut s, 1);
            ffi::vaTerminate(self.dpy);
            libc::close(self.render_fd);
        }
    }
}
