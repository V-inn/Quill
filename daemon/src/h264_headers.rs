//! Hand-built H.264 SPS/PPS RBSP bytes. The iHD VAAPI driver's low-power
//! encode entrypoint does not emit these itself -- it only produces slice
//! data -- so the caller is expected to supply them via VAAPI "packed
//! header" buffers. Values here must exactly match the fields set on
//! VAEncSequenceParameterBufferH264 / VAEncPictureParameterBufferH264 in
//! vaapi_encoder.rs.

struct BitWriter {
    bytes: Vec<u8>,
    cur: u8,
    nbits: u32, // bits filled in `cur`, MSB-first
}

impl BitWriter {
    fn new() -> Self {
        Self { bytes: Vec::new(), cur: 0, nbits: 0 }
    }

    fn write_bit(&mut self, bit: u32) {
        self.cur = (self.cur << 1) | (bit as u8 & 1);
        self.nbits += 1;
        if self.nbits == 8 {
            self.bytes.push(self.cur);
            self.cur = 0;
            self.nbits = 0;
        }
    }

    fn write_bits(&mut self, value: u32, n: u32) {
        for i in (0..n).rev() {
            self.write_bit((value >> i) & 1);
        }
    }

    /// Unsigned Exp-Golomb (ue(v)).
    fn write_ue(&mut self, value: u32) {
        let code = value + 1;
        let nbits = 32 - code.leading_zeros();
        for _ in 0..nbits - 1 {
            self.write_bit(0);
        }
        self.write_bits(code, nbits);
    }

    /// Signed Exp-Golomb (se(v)).
    fn write_se(&mut self, value: i32) {
        let mapped = if value <= 0 {
            (-value) as u32 * 2
        } else {
            value as u32 * 2 - 1
        };
        self.write_ue(mapped);
    }

    /// rbsp_trailing_bits(): stop bit + zero padding to a byte boundary.
    fn finish(mut self) -> Vec<u8> {
        self.write_bit(1);
        while self.nbits != 0 {
            self.write_bit(0);
        }
        self.bytes
    }
}

/// Inserts emulation_prevention_three_byte (0x03) per H.264 Annex B: any
/// 0x00 0x00 followed by a byte <= 0x03 gets a 0x03 spliced in before it.
fn apply_emulation_prevention(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 100 + 4);
    let mut zero_run = 0;
    for &b in data {
        if zero_run >= 2 && b <= 0x03 {
            out.push(0x03);
            zero_run = 0;
        }
        out.push(b);
        if b == 0 {
            zero_run += 1;
        } else {
            zero_run = 0;
        }
    }
    out
}

pub struct H264Params {
    pub profile_idc: u8, // 66 = Constrained Baseline
    pub level_idc: u8,
    pub mbs_width: u32,
    pub mbs_height: u32,
    pub max_num_ref_frames: u32,
    pub log2_max_frame_num_minus4: u32,
    pub log2_max_pic_order_cnt_lsb_minus4: u32,
    pub frame_crop_right: u32,
    pub frame_crop_bottom: u32,
    pub pic_init_qp: u8,
    pub deblocking_filter_control_present: bool,
    /// VUI `max_dec_frame_buffering`. Must be >= `max_num_ref_frames` -- some
    /// decoders reject the stream outright otherwise, so this is derived from
    /// the same value in `vaapi_encoder.rs` rather than being an independent
    /// constant that can silently drift.
    pub max_dec_frame_buffering: u32,
}

/// Returns the SPS NAL unit's RBSP payload (NAL header byte + body), not
/// including the Annex-B start code -- VAAPI's packed-header mechanism adds
/// that itself.
pub fn build_sps(p: &H264Params) -> Vec<u8> {
    let mut nal = vec![0x67u8]; // nal_ref_idc=3, nal_unit_type=7 (SPS)
    let mut w = BitWriter::new();
    w.write_bits(p.profile_idc as u32, 8);
    w.write_bits(0x00, 8); // constraint_set flags: none claimed, plain Main profile
    w.write_bits(p.level_idc as u32, 8);
    w.write_ue(0); // seq_parameter_set_id
    // chroma_format_idc/bit_depth fields omitted: only present in the SPS for
    // profile_idc in the High-profile family (100/110/122/...), not for
    // Main (77) or Baseline (66).
    w.write_ue(p.log2_max_frame_num_minus4);
    w.write_ue(0); // pic_order_cnt_type
    w.write_ue(p.log2_max_pic_order_cnt_lsb_minus4);
    w.write_ue(p.max_num_ref_frames);
    w.write_bit(0); // gaps_in_frame_num_value_allowed_flag
    w.write_ue(p.mbs_width - 1); // pic_width_in_mbs_minus1
    w.write_ue(p.mbs_height - 1); // pic_height_in_map_units_minus1 (frame_mbs_only=1)
    w.write_bit(1); // frame_mbs_only_flag
    w.write_bit(1); // direct_8x8_inference_flag
    let crop = p.frame_crop_right > 0 || p.frame_crop_bottom > 0;
    w.write_bit(crop as u32);
    if crop {
        w.write_ue(0); // left
        w.write_ue(p.frame_crop_right);
        w.write_ue(0); // top
        w.write_ue(p.frame_crop_bottom);
    }
    write_low_latency_vui(&mut w, p);
    nal.extend(w.finish());
    with_start_code(apply_emulation_prevention(&nal))
}

/// The whole reason this SPS carries a VUI at all: `bitstream_restriction_flag`
/// with `max_num_reorder_frames = 0`.
///
/// Without a VUI, a decoder has no way to know this stream never reorders, so
/// clause E.2.1 makes it infer `max_num_reorder_frames = MaxDpbFrames`, derived
/// from `level_idc` and the picture size (Annex A, Table A-1). At level 4.1
/// (`MaxDpbMbs = 32768`) that's 2 frames for 2560x1600 (160x100 = 16000 MBs) and
/// 4 frames for 1920x1080 (120x68 = 8160 MBs) -- which is exactly the
/// `pending=3` / `pending=4-5` decoder queue depth the Android client has been
/// logging at those two resolutions all along (see MILESTONES.md, Milestone 7).
/// At 60fps that is 33-66ms of latency the decoder is *required* to add, and it
/// explains why every decoder-side flag tried in Milestone 7 (`KEY_LOW_LATENCY`,
/// the Exynos vendor key, `KEY_PRIORITY`) measured as a no-op: they were all
/// fighting an instruction carried in the bitstream itself. This is not a
/// hardware floor, as that milestone concluded.
///
/// Same fields, same values, as two mature projects solving the same problem:
/// Sunshine rewrites its hardware encoder's SPS this way host-side
/// (`src/cbs.cpp`, `make_sps_h264`), and moonlight-android patches it
/// client-side on devices whose encoders omit it
/// (`MediaCodecDecoderRenderer.java`, "increases decoding latency"). Quill owns
/// both ends, so emitting it correctly here means the client needs no patching.
///
/// Everything ahead of `bitstream_restriction_flag` is signalled absent.
/// `timing_info` in particular is deliberately *not* written: it would mean
/// hardcoding a frame rate here, the capture side has no fixed one to report
/// (frames arrive on damage), and with `fixed_frame_rate_flag` off it carries no
/// normative weight anyway.
fn write_low_latency_vui(w: &mut BitWriter, p: &H264Params) {
    w.write_bit(1); // vui_parameters_present_flag
    w.write_bit(0); // aspect_ratio_info_present_flag
    w.write_bit(0); // overscan_info_present_flag
    w.write_bit(0); // video_signal_type_present_flag
    w.write_bit(0); // chroma_loc_info_present_flag
    w.write_bit(0); // timing_info_present_flag
    w.write_bit(0); // nal_hrd_parameters_present_flag
    w.write_bit(0); // vcl_hrd_parameters_present_flag
    w.write_bit(0); // pic_struct_present_flag
    w.write_bit(1); // bitstream_restriction_flag
    w.write_bit(1); // motion_vectors_over_pic_boundaries_flag
    w.write_ue(0); // max_bytes_per_pic_denom (0 = no limit signalled)
    w.write_ue(0); // max_bits_per_mb_denom (0 = no limit signalled)
    w.write_ue(16); // log2_max_mv_length_horizontal
    w.write_ue(16); // log2_max_mv_length_vertical
    w.write_ue(0); // max_num_reorder_frames -- the point of all this
    w.write_ue(p.max_dec_frame_buffering);
}

/// Returns the PPS NAL unit's RBSP payload (NAL header byte + body).
pub fn build_pps(p: &H264Params) -> Vec<u8> {
    let mut nal = vec![0x68u8]; // nal_ref_idc=3, nal_unit_type=8 (PPS)
    let mut w = BitWriter::new();
    w.write_ue(0); // pic_parameter_set_id
    w.write_ue(0); // seq_parameter_set_id
    w.write_bit(1); // entropy_coding_mode_flag (CABAC) -- must match pic.pic_fields in vaapi_encoder.rs
    w.write_bit(0); // bottom_field_pic_order_in_frame_present_flag
    w.write_ue(0); // num_slice_groups_minus1
    w.write_ue(0); // num_ref_idx_l0_default_active_minus1
    w.write_ue(0); // num_ref_idx_l1_default_active_minus1
    w.write_bit(0); // weighted_pred_flag
    w.write_bits(0, 2); // weighted_bipred_idc
    w.write_se(p.pic_init_qp as i32 - 26); // pic_init_qp_minus26
    w.write_se(p.pic_init_qp as i32 - 26); // pic_init_qs_minus26
    w.write_se(0); // chroma_qp_index_offset
    w.write_bit(p.deblocking_filter_control_present as u32);
    w.write_bit(0); // constrained_intra_pred_flag
    w.write_bit(0); // redundant_pic_cnt_present_flag
    nal.extend(w.finish());
    with_start_code(apply_emulation_prevention(&nal))
}

/// Prepends the Annex-B long start code. Must happen after emulation
/// prevention, never before -- the literal `00 00 01` here must not be
/// mistaken by that pass for RBSP content needing an escape byte.
fn with_start_code(mut nal: Vec<u8>) -> Vec<u8> {
    let mut out = vec![0x00, 0x00, 0x00, 0x01];
    out.append(&mut nal);
    out
}
