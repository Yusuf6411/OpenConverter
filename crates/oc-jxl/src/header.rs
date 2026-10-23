//! Image and frame headers, the table of contents and the ICC stream skipper.
use crate::bits::{bad, unpack_signed, Br};
use crate::entropy::{read_permutation, Decoder};
use oc_core::{Error, ErrorCode, Result};

fn unsupported(msg: &str) -> Error {
    Error::new(ErrorCode::UnsupportedVariant, format!("JPEG XL: {msg}"))
}

fn enum_checked(br: &mut Br, allowed: &[u32], what: &str) -> Result<u32> {
    let v = br.enum_value()?;
    if allowed.contains(&v) {
        Ok(v)
    } else {
        Err(bad(&format!("invalid {what} {v}")))
    }
}

// ---------------------------------------------------------------------------------------------
// Image header

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Space {
    Rgb,
    Grey,
    Xyb,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum White {
    D65,
    E,
    Dci,
    Custom([f64; 2]),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Prim {
    Srgb,
    Bt2100,
    P3,
    Custom([[f64; 2]; 3]),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tf {
    /// Pure power law: the value is the exponent applied to decode (`encoded^g`).
    Gamma(f64),
    Bt709,
    Unknown,
    Linear,
    Srgb,
    Pq,
    Dci,
    Hlg,
}

#[derive(Clone, Debug)]
pub struct Colour {
    pub want_icc: bool,
    pub space: Space,
    pub white: White,
    pub prim: Prim,
    pub tf: Tf,
}

impl Colour {
    fn default_srgb() -> Self {
        Self {
            want_icc: false,
            space: Space::Rgb,
            white: White::D65,
            prim: Prim::Srgb,
            tf: Tf::Srgb,
        }
    }

    fn parse(br: &mut Br) -> Result<Self> {
        if br.bool()? {
            return Ok(Self::default_srgb());
        }
        let want_icc = br.bool()?;
        let space = match enum_checked(br, &[0, 1, 2, 3], "colour space")? {
            0 => Space::Rgb,
            1 => Space::Grey,
            2 => Space::Xyb,
            _ => Space::Unknown,
        };
        if want_icc {
            return Ok(Self {
                want_icc,
                space,
                ..Self::default_srgb()
            });
        }
        fn xy(br: &mut Br) -> Result<[f64; 2]> {
            let mut c = [0f64; 2];
            for v in &mut c {
                let raw = br.u32([(0, 19), (524_288, 19), (1_048_576, 20), (2_097_152, 21)])?;
                *v = f64::from(unpack_signed(raw)) / 1e6;
            }
            Ok(c)
        }
        let white = if space == Space::Xyb {
            White::D65
        } else {
            match enum_checked(br, &[1, 2, 10, 11], "white point")? {
                1 => White::D65,
                2 => White::Custom(xy(br)?),
                10 => White::E,
                _ => White::Dci,
            }
        };
        let prim = if matches!(space, Space::Xyb | Space::Grey) {
            Prim::Srgb
        } else {
            match enum_checked(br, &[1, 2, 9, 11], "primaries")? {
                1 => Prim::Srgb,
                2 => Prim::Custom([xy(br)?, xy(br)?, xy(br)?]),
                9 => Prim::Bt2100,
                _ => Prim::P3,
            }
        };
        let tf = if br.bool()? {
            Tf::Gamma(f64::from(br.read(24)?) / 1e7)
        } else {
            match enum_checked(br, &[1, 2, 8, 13, 16, 17, 18], "transfer function")? {
                1 => Tf::Bt709,
                2 => Tf::Unknown,
                8 => Tf::Linear,
                13 => Tf::Srgb,
                16 => Tf::Pq,
                17 => Tf::Dci,
                _ => Tf::Hlg,
            }
        };
        enum_checked(br, &[0, 1, 2, 3], "rendering intent")?;
        Ok(Self {
            want_icc,
            space,
            white,
            prim,
            tf,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BitDepth {
    pub bits: u32,
    /// Exponent bits for floating-point samples.
    pub exp_bits: Option<u32>,
}

impl BitDepth {
    fn parse(br: &mut Br) -> Result<Self> {
        if br.bool()? {
            let bits = br.u32([(32, 0), (16, 0), (24, 0), (1, 6)])?;
            let exp_bits = br.read(4)? + 1;
            if !(2..=8).contains(&exp_bits) {
                return Err(bad("invalid exponent bits"));
            }
            let mantissa = bits.wrapping_sub(exp_bits + 1);
            if !(2..=23).contains(&mantissa) {
                return Err(bad("invalid mantissa bits"));
            }
            Ok(Self {
                bits,
                exp_bits: Some(exp_bits),
            })
        } else {
            let bits = br.u32([(8, 0), (10, 0), (12, 0), (1, 6)])?;
            if bits > 31 {
                return Err(bad("invalid bit depth"));
            }
            Ok(Self {
                bits,
                exp_bits: None,
            })
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EcType {
    Alpha { associated: bool },
    Depth,
    Spot,
    Mask,
    Black,
    Cfa,
    Thermal,
    Other,
}

#[derive(Clone, Debug)]
pub struct ExtraChannel {
    pub ty: EcType,
    pub bit_depth: BitDepth,
    pub dim_shift: u32,
}

impl ExtraChannel {
    fn parse(br: &mut Br) -> Result<Self> {
        if br.bool()? {
            return Ok(Self {
                ty: EcType::Alpha { associated: false },
                bit_depth: BitDepth {
                    bits: 8,
                    exp_bits: None,
                },
                dim_shift: 0,
            });
        }
        let id = enum_checked(br, &[0, 1, 2, 3, 4, 5, 6, 15, 16], "extra channel type")?;
        let bit_depth = BitDepth::parse(br)?;
        let dim_shift = br.u32([(0, 0), (3, 0), (4, 0), (1, 3)])?;
        read_name(br)?;
        let ty = match id {
            0 => EcType::Alpha {
                associated: br.bool()?,
            },
            1 => EcType::Depth,
            2 => {
                for _ in 0..4 {
                    br.f16()?;
                }
                EcType::Spot
            }
            3 => EcType::Mask,
            4 => EcType::Black,
            5 => {
                br.u32([(1, 0), (0, 2), (3, 4), (19, 8)])?;
                EcType::Cfa
            }
            6 => EcType::Thermal,
            _ => EcType::Other,
        };
        Ok(Self {
            ty,
            bit_depth,
            dim_shift,
        })
    }
}

fn read_name(br: &mut Br) -> Result<()> {
    let len = br.u32([(0, 0), (0, 4), (16, 5), (48, 10)])?;
    br.skip(len as usize * 8)
}

fn skip_extensions(br: &mut Br) -> Result<()> {
    let mask = br.u64()?;
    let mut total = 0u64;
    for i in 0..64 {
        if mask >> i & 1 != 0 {
            total = total.saturating_add(br.u64()?);
        }
    }
    if total > br.bits_left() as u64 {
        return Err(crate::bits::eof());
    }
    br.skip(total as usize)
}

#[derive(Clone, Debug)]
pub struct Opsin {
    pub inv_mat: [[f32; 3]; 3],
    pub bias: [f32; 3],
    pub quant_bias: [f32; 3],
    pub quant_bias_num: f32,
}

impl Opsin {
    fn default_matrix() -> Self {
        Self {
            inv_mat: [
                [11.031_567, -9.866_944, -0.164_623],
                [-3.254_147_5, 4.418_770_3, -0.164_623],
                [-3.658_851_4, 2.712_923, 1.945_928_3],
            ],
            bias: [-0.003_793_073_3; 3],
            quant_bias: [0.945_349_93, 0.929_945_5, 0.950_064_9],
            quant_bias_num: 0.145,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Animation {
    pub tps_num: u32,
    pub tps_den: u32,
    pub num_loops: u32,
    pub have_timecodes: bool,
}

#[derive(Clone, Debug)]
pub struct ImageHeader {
    pub width: u32,
    pub height: u32,
    pub orientation: u32,
    pub preview: Option<(u32, u32)>,
    pub animation: Option<Animation>,
    pub bit_depth: BitDepth,
    pub ec: Vec<ExtraChannel>,
    pub xyb_encoded: bool,
    pub colour: Colour,
    /// Peak luminance in nits that the sample value 1.0 corresponds to for HDR content.
    pub intensity_target: f32,
    pub opsin: Opsin,
    /// Custom upsampling kernels, when the file carries them.
    pub custom_up_weights: bool,
}

fn size_header(br: &mut Br) -> Result<(u32, u32)> {
    let div8 = br.bool()?;
    let h_div8 = if div8 { br.read(5)? + 1 } else { 0 };
    let big = [(1, 9), (1, 13), (1, 18), (1, 30)];
    let height = if div8 { 8 * h_div8 } else { br.u32(big)? };
    let ratio = br.read(3)?;
    let w_div8 = if div8 && ratio == 0 {
        br.read(5)? + 1
    } else {
        0
    };
    let width = if !div8 && ratio == 0 {
        br.u32(big)?
    } else {
        let h = u64::from(height);
        (match ratio {
            0 => 8 * u64::from(w_div8),
            1 => h,
            2 => h * 12 / 10,
            3 => h * 4 / 3,
            4 => h * 3 / 2,
            5 => h * 16 / 9,
            6 => h * 5 / 4,
            _ => h * 2,
        }) as u32
    };
    Ok((width, height))
}

fn preview_header(br: &mut Br) -> Result<(u32, u32)> {
    let div8 = br.bool()?;
    let dim = [(1, 6), (65, 8), (321, 10), (1345, 12)];
    let div = [(16, 0), (32, 0), (1, 5), (33, 9)];
    let h_div8 = if div8 { br.u32(div)? } else { 0 };
    let height = if div8 { 8 * h_div8 } else { br.u32(dim)? };
    let ratio = br.read(3)?;
    let w_div8 = if div8 && ratio == 0 { br.u32(div)? } else { 0 };
    let width = if !div8 && ratio == 0 {
        br.u32(dim)?
    } else {
        let h = u64::from(height);
        (match ratio {
            0 => 8 * u64::from(w_div8),
            1 => h,
            2 => h * 12 / 10,
            3 => h * 4 / 3,
            4 => h * 3 / 2,
            5 => h * 16 / 9,
            6 => h * 5 / 4,
            _ => h * 2,
        }) as u32
    };
    Ok((width, height))
}

impl ImageHeader {
    /// Parse the image header after the two signature bytes.
    pub fn parse(br: &mut Br) -> Result<Self> {
        if br.read(16)? != 0x0aff {
            return Err(bad("signature mismatch"));
        }
        let (width, height) = size_header(br)?;
        if width == 0 || height == 0 {
            return Err(bad("empty image"));
        }
        let all_default = br.bool()?;
        let mut orientation = 1;
        let mut preview = None;
        let mut animation = None;
        let mut bit_depth = BitDepth {
            bits: 8,
            exp_bits: None,
        };
        let mut ec = Vec::new();
        let mut xyb_encoded = true;
        let mut intensity_target = 255.0f32;
        let mut colour = Colour::default_srgb();
        if !all_default {
            let extra_fields = br.bool()?;
            if extra_fields {
                orientation = br.read(3)? + 1;
                if br.bool()? {
                    size_header(br)?;
                }
                if br.bool()? {
                    preview = Some(preview_header(br)?);
                }
                if br.bool()? {
                    animation = Some(Animation {
                        tps_num: br.u32([(100, 0), (1000, 0), (1, 10), (1, 30)])?,
                        tps_den: br.u32([(1, 0), (1001, 0), (1, 8), (1, 10)])?,
                        num_loops: br.u32([(0, 0), (0, 3), (0, 16), (0, 32)])?,
                        have_timecodes: br.bool()?,
                    });
                }
            }
            bit_depth = BitDepth::parse(br)?;
            br.bool()?; // modular_16bit_buffers
            let num_extra = br.u32([(0, 0), (1, 0), (2, 4), (1, 12)])?;
            if num_extra > 256 {
                return Err(bad("too many extra channels"));
            }
            for _ in 0..num_extra {
                ec.push(ExtraChannel::parse(br)?);
            }
            xyb_encoded = br.bool()?;
            colour = Colour::parse(br)?;
            if extra_fields && !br.bool()? {
                let intensity = br.f16()?;
                intensity_target = intensity;
                let min_nits = br.f16()?;
                let relative = br.bool()?;
                let linear_below = br.f16()?;
                if intensity <= 0.0
                    || min_nits < 0.0
                    || min_nits > intensity
                    || linear_below < 0.0
                    || (relative && linear_below > 1.0)
                {
                    return Err(bad("invalid tone mapping"));
                }
            }
            skip_extensions(br)?;
        }
        let default_m = br.bool()?;
        let mut opsin = Opsin::default_matrix();
        let mut custom_up_weights = false;
        if !default_m {
            if xyb_encoded && !br.bool()? {
                for row in &mut opsin.inv_mat {
                    for v in row {
                        *v = br.f16()?;
                    }
                }
                for v in &mut opsin.bias {
                    *v = br.f16()?;
                }
                for v in &mut opsin.quant_bias {
                    *v = br.f16()?;
                }
                opsin.quant_bias_num = br.f16()?;
            }
            let mask = br.read(3)?;
            for (bit, count) in [(1, 15), (2, 55), (4, 210)] {
                if mask & bit != 0 {
                    custom_up_weights = true;
                    for _ in 0..count {
                        br.f16()?;
                    }
                }
            }
        }
        Ok(Self {
            width,
            height,
            orientation,
            preview,
            animation,
            bit_depth,
            ec,
            xyb_encoded,
            colour,
            intensity_target,
            opsin,
            custom_up_weights,
        })
    }

    pub fn is_grey(&self) -> bool {
        self.colour.space == Space::Grey
    }
}

// ---------------------------------------------------------------------------------------------
// The encoded ICC profile: decoded only far enough to step over it

fn icc_ctx(idx: usize, b1: u8, b2: u8) -> u32 {
    if idx <= 128 {
        return 0;
    }
    let p1 = match b1 {
        b'a'..=b'z' | b'A'..=b'Z' => 0,
        b'0'..=b'9' | b'.' | b',' => 1,
        0..=1 => 2 + u32::from(b1),
        2..=15 => 4,
        241..=254 => 5,
        255 => 6,
        _ => 7,
    };
    let p2 = match b2 {
        b'a'..=b'z' | b'A'..=b'Z' => 0,
        b'0'..=b'9' | b'.' | b',' => 1,
        0..=15 => 2,
        241..=255 => 3,
        _ => 4,
    };
    1 + p1 + 8 * p2
}

/// Read (and discard) the compressed ICC profile that follows the image header.
pub fn skip_icc(br: &mut Br) -> Result<()> {
    let size = br.u64()?;
    if size > 1 << 28 {
        return Err(Error::limit("alloc", "JPEG XL: ICC profile is too large"));
    }
    let mut dec = Decoder::parse(br, 41)?;
    dec.begin(br)?;
    let (mut b1, mut b2) = (0u8, 0u8);
    for idx in 0..size as usize {
        let sym = dec.read(br, icc_ctx(idx, b1, b2))?;
        if sym >= 256 {
            return Err(bad("ICC symbol out of range"));
        }
        b2 = b1;
        b1 = sym as u8;
    }
    dec.finalize()
}

// ---------------------------------------------------------------------------------------------
// Frame header

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameType {
    Regular,
    Lf,
    ReferenceOnly,
    SkipProgressive,
}

impl FrameType {
    pub fn is_normal(self) -> bool {
        matches!(self, Self::Regular | Self::SkipProgressive)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blend {
    Replace,
    Add,
    Blend,
    MulAdd,
    Mul,
}

#[derive(Clone, Copy, Debug)]
pub struct BlendInfo {
    pub mode: Blend,
    pub alpha_channel: u32,
    pub clamp: bool,
    pub source: u32,
}

#[derive(Clone, Debug)]
pub struct Passes {
    pub num_passes: u32,
    pub shift: Vec<u32>,
    pub downsample: Vec<u32>,
    pub last_pass: Vec<u32>,
}

#[derive(Clone, Debug)]
pub struct Epf {
    pub iters: u32,
    pub sharp_lut: [f32; 8],
    pub channel_scale: [f32; 3],
    pub quant_mul: f32,
    pub pass0_sigma_scale: f32,
    pub pass2_sigma_scale: f32,
    pub border_sad_mul: f32,
    pub sigma_for_modular: f32,
}

#[derive(Clone, Debug)]
pub struct FrameHeader {
    pub frame_type: FrameType,
    pub modular: bool,
    pub flags: u64,
    pub do_ycbcr: bool,
    pub encoded_color_channels: usize,
    pub jpeg_upsampling: [u32; 3],
    pub upsampling: u32,
    pub ec_upsampling: Vec<u32>,
    pub group_size_shift: u32,
    pub x_qm_scale: u32,
    pub b_qm_scale: u32,
    pub passes: Passes,
    pub lf_level: u32,
    pub x0: i32,
    pub y0: i32,
    pub width: u32,
    pub height: u32,
    pub blending: BlendInfo,
    pub ec_blending: Vec<BlendInfo>,
    pub duration: u32,
    pub is_last: bool,
    pub save_as_reference: u32,
    pub save_before_ct: bool,
    pub gab: Option<[[f32; 2]; 3]>,
    pub epf: Option<Epf>,
}

pub const FLAG_NOISE: u64 = 1;
pub const FLAG_PATCHES: u64 = 2;
pub const FLAG_SPLINES: u64 = 0x10;
pub const FLAG_USE_LF_FRAME: u64 = 0x20;
pub const FLAG_SKIP_ADAPTIVE_LF: u64 = 0x80;

fn blend_info(
    br: &mut Br,
    has_ec: bool,
    canvas_full: impl Fn(Blend) -> bool,
    prev_mode: Option<Blend>,
) -> Result<BlendInfo> {
    let mode = match br.u32([(0, 0), (1, 0), (2, 0), (3, 2)])? {
        0 => Blend::Replace,
        1 => Blend::Add,
        2 => Blend::Blend,
        3 => Blend::MulAdd,
        4 => Blend::Mul,
        _ => return Err(bad("invalid blend mode")),
    };
    let uses_alpha = has_ec && matches!(mode, Blend::Blend | Blend::MulAdd);
    let alpha_channel = if uses_alpha {
        br.u32([(0, 0), (1, 0), (2, 0), (3, 3)])?
    } else {
        0
    };
    let clamp = if uses_alpha || mode == Blend::Mul {
        br.bool()?
    } else {
        false
    };
    let source = if !canvas_full(prev_mode.unwrap_or(mode)) {
        br.read(2)?
    } else {
        0
    };
    Ok(BlendInfo {
        mode,
        alpha_channel,
        clamp,
        source,
    })
}

impl FrameHeader {
    pub fn parse(br: &mut Br, img: &ImageHeader) -> Result<Self> {
        let all_default = br.bool()?;
        let mut h = Self {
            frame_type: FrameType::Regular,
            modular: false,
            flags: 0,
            do_ycbcr: false,
            encoded_color_channels: 3,
            jpeg_upsampling: [0; 3],
            upsampling: 1,
            ec_upsampling: vec![1; img.ec.len()],
            group_size_shift: 1,
            x_qm_scale: 3,
            b_qm_scale: 2,
            passes: Passes {
                num_passes: 1,
                shift: Vec::new(),
                downsample: Vec::new(),
                last_pass: Vec::new(),
            },
            lf_level: 0,
            x0: 0,
            y0: 0,
            width: img.width,
            height: img.height,
            blending: BlendInfo {
                mode: Blend::Replace,
                alpha_channel: 0,
                clamp: false,
                source: 0,
            },
            ec_blending: vec![
                BlendInfo {
                    mode: Blend::Replace,
                    alpha_channel: 0,
                    clamp: false,
                    source: 0
                };
                img.ec.len()
            ],
            duration: 0,
            is_last: true,
            save_as_reference: 0,
            save_before_ct: false,
            gab: Some([[0.115_169_525, 0.061_248_592]; 3]),
            epf: Some(Epf {
                iters: 2,
                sharp_lut: [
                    0.0,
                    1.0 / 7.0,
                    2.0 / 7.0,
                    3.0 / 7.0,
                    4.0 / 7.0,
                    5.0 / 7.0,
                    6.0 / 7.0,
                    1.0,
                ],
                channel_scale: [40.0, 5.0, 3.5],
                quant_mul: 0.46,
                pass0_sigma_scale: 0.9,
                pass2_sigma_scale: 6.5,
                border_sad_mul: 2.0 / 3.0,
                sigma_for_modular: 1.0,
            }),
        };
        if all_default {
            return Ok(h);
        }
        h.frame_type = match br.read(2)? {
            0 => FrameType::Regular,
            1 => FrameType::Lf,
            2 => FrameType::ReferenceOnly,
            _ => FrameType::SkipProgressive,
        };
        h.modular = br.read(1)? == 1;
        h.flags = br.u64()?;
        if !img.xyb_encoded {
            h.do_ycbcr = br.bool()?;
        }
        h.encoded_color_channels = if h.modular && !h.do_ycbcr && !img.xyb_encoded && img.is_grey()
        {
            1
        } else {
            3
        };
        let use_lf_frame = h.flags & FLAG_USE_LF_FRAME != 0;
        if h.do_ycbcr && !use_lf_frame {
            for v in &mut h.jpeg_upsampling {
                *v = br.read(2)?;
            }
        }
        if !use_lf_frame {
            h.upsampling = br.u32([(1, 0), (2, 0), (4, 0), (8, 0)])?;
            for v in &mut h.ec_upsampling {
                *v = br.u32([(1, 0), (2, 0), (4, 0), (8, 0)])?;
            }
        }
        if h.modular {
            h.group_size_shift = br.read(2)?;
        }
        h.x_qm_scale = if img.xyb_encoded && !h.modular { 3 } else { 2 };
        if img.xyb_encoded && !h.modular {
            h.x_qm_scale = br.read(3)?;
            h.b_qm_scale = br.read(3)?;
        }
        if h.frame_type != FrameType::ReferenceOnly {
            let num_passes = br.u32([(1, 0), (2, 0), (3, 0), (4, 3)])?;
            let mut p = Passes {
                num_passes,
                shift: Vec::new(),
                downsample: Vec::new(),
                last_pass: Vec::new(),
            };
            if num_passes != 1 {
                let num_ds = br.u32([(0, 0), (1, 0), (2, 0), (3, 1)])?;
                if num_ds >= num_passes {
                    return Err(bad("too many downsample entries"));
                }
                for _ in 0..num_passes - 1 {
                    p.shift.push(br.read(2)?);
                }
                for _ in 0..num_ds {
                    p.downsample.push(br.u32([(1, 0), (2, 0), (4, 0), (8, 0)])?);
                }
                for _ in 0..num_ds {
                    p.last_pass.push(br.u32([(0, 0), (1, 0), (2, 0), (0, 3)])?);
                }
            }
            h.passes = p;
        }
        if h.frame_type == FrameType::Lf {
            h.lf_level = br.read(2)? + 1;
        }
        let mut have_crop = false;
        if h.frame_type != FrameType::Lf {
            have_crop = br.bool()?;
        }
        if have_crop {
            let signed = [(0, 8), (256, 11), (2304, 14), (18688, 30)];
            if h.frame_type != FrameType::ReferenceOnly {
                h.x0 = unpack_signed(br.u32(signed)?);
                h.y0 = unpack_signed(br.u32(signed)?);
            }
            h.width = br.u32(signed)?;
            h.height = br.u32(signed)?;
        }
        let full_canvas = |h: &Self, mode: Blend| -> bool {
            mode == Blend::Replace
                && (!have_crop
                    || (h.x0 <= 0
                        && h.y0 <= 0
                        && i64::from(h.x0) + i64::from(h.width) >= i64::from(img.width)
                        && i64::from(h.y0) + i64::from(h.height) >= i64::from(img.height)))
        };
        let has_ec = !img.ec.is_empty();
        let mut resets_canvas = true;
        if h.frame_type.is_normal() {
            let snapshot = h.clone();
            h.blending = blend_info(br, has_ec, |m| full_canvas(&snapshot, m), None)?;
            let first_mode = h.blending.mode;
            for i in 0..img.ec.len() {
                h.ec_blending[i] =
                    blend_info(br, has_ec, |m| full_canvas(&snapshot, m), Some(first_mode))?;
            }
            resets_canvas = full_canvas(&snapshot, first_mode);
            if img.animation.is_some() {
                h.duration = br.u32([(0, 0), (1, 0), (0, 8), (0, 32)])?;
            }
            if img.animation.as_ref().is_some_and(|a| a.have_timecodes) {
                br.read(32)?;
            }
            h.is_last = br.bool()?;
        } else {
            h.is_last = false;
            resets_canvas = full_canvas(&h.clone(), h.blending.mode);
        }
        if h.frame_type != FrameType::Lf && !h.is_last {
            h.save_as_reference = br.read(2)?;
        }
        h.save_before_ct = !h.frame_type.is_normal();
        if h.frame_type == FrameType::ReferenceOnly
            || (resets_canvas
                && !h.is_last
                && (h.duration == 0 || h.save_as_reference != 0)
                && h.frame_type != FrameType::Lf)
        {
            h.save_before_ct = br.bool()?;
        }
        read_name(br)?;
        // Restoration filter
        let all_default_filter = br.bool()?;
        if !all_default_filter {
            let gab_enabled = br.bool()?;
            h.gab = None;
            if gab_enabled {
                if br.bool()? {
                    let mut w = [[0f32; 2]; 3];
                    for c in &mut w {
                        c[0] = br.f16()?;
                        c[1] = br.f16()?;
                        if (1.0 + (c[0] + c[1]) * 4.0).abs() < f32::EPSILON {
                            return Err(bad("gaborish weights are degenerate"));
                        }
                    }
                    h.gab = Some(w);
                } else {
                    h.gab = Some([[0.115_169_525, 0.061_248_592]; 3]);
                }
            }
            let iters = br.read(2)?;
            if iters == 0 {
                h.epf = None;
            } else {
                let mut e = h.epf.take().unwrap();
                e.iters = iters;
                if !h.modular && br.bool()? {
                    for v in &mut e.sharp_lut {
                        *v = br.f16()?;
                    }
                }
                if br.bool()? {
                    for v in &mut e.channel_scale {
                        *v = br.f16()?;
                    }
                    br.read(32)?;
                }
                if br.bool()? {
                    if !h.modular {
                        e.quant_mul = br.f16()?;
                    }
                    e.pass0_sigma_scale = br.f16()?;
                    e.pass2_sigma_scale = br.f16()?;
                    e.border_sad_mul = br.f16()?;
                }
                if h.modular {
                    e.sigma_for_modular = br.f16()?;
                }
                h.epf = Some(e);
            }
            skip_extensions(br)?;
        }
        skip_extensions(br)?;
        if h.width == 0 || h.height == 0 {
            return Err(bad("frame has no pixels"));
        }
        if h.flags & FLAG_USE_LF_FRAME != 0 && h.lf_level >= 4 {
            return Err(bad("lf_level out of range"));
        }
        let _ = unsupported;
        Ok(h)
    }

    /// Whether this frame is one that is displayed.
    pub fn is_keyframe(&self) -> bool {
        self.frame_type.is_normal() && (self.is_last || self.duration != 0)
    }

    /// Whether the frame is stored in a reference slot for later frames.
    pub fn can_reference(&self) -> bool {
        !self.is_last
            && (self.duration == 0 || self.save_as_reference != 0)
            && self.frame_type != FrameType::Lf
    }

    pub fn use_lf_frame(&self) -> bool {
        self.flags & FLAG_USE_LF_FRAME != 0
    }

    fn sample_dim(&self, v: u32, up: u32) -> u32 {
        let mut v = v;
        if up > 1 {
            v = v.div_ceil(up);
        }
        if self.lf_level > 0 {
            let s = 3 * self.lf_level;
            v = (v + (1 << s) - 1) >> s;
        }
        v
    }

    pub fn color_w(&self) -> u32 {
        self.sample_dim(self.width, self.upsampling)
    }

    pub fn color_h(&self) -> u32 {
        self.sample_dim(self.height, self.upsampling)
    }

    pub fn group_dim(&self) -> u32 {
        128 << self.group_size_shift
    }

    pub fn groups_per_row(&self) -> u32 {
        self.color_w().div_ceil(self.group_dim())
    }

    pub fn num_groups(&self) -> u32 {
        self.groups_per_row() * self.color_h().div_ceil(self.group_dim())
    }

    pub fn lf_group_dim(&self) -> u32 {
        self.group_dim() * 8
    }

    pub fn lf_groups_per_row(&self) -> u32 {
        self.color_w().div_ceil(self.lf_group_dim())
    }

    pub fn num_lf_groups(&self) -> u32 {
        self.lf_groups_per_row() * self.color_h().div_ceil(self.lf_group_dim())
    }

    /// Size in samples of group `idx` for groups of `dim` (the last row/column may be short).
    pub fn group_size(&self, dim: u32, idx: u32) -> (u32, u32) {
        let (w, h) = (self.color_w(), self.color_h());
        let per_row = w.div_ceil(dim);
        let (col, row) = (idx % per_row, idx / per_row);
        ((w - col * dim).min(dim), (h - row * dim).min(dim))
    }

    /// `(min_shift, max_shift)` of the modular channels each pass carries.
    pub fn pass_shifts(&self) -> Vec<(u32, (i32, i32))> {
        let mut out: Vec<(u32, (i32, i32))> = Vec::new();
        let mut max_shift = 3i32;
        for (&ds, &last) in self.passes.downsample.iter().zip(&self.passes.last_pass) {
            let min_shift = ds.trailing_zeros() as i32;
            out.push((last, (min_shift, max_shift)));
            max_shift = min_shift;
        }
        out.push((self.passes.num_passes - 1, (0, max_shift)));
        out
    }
}

// ---------------------------------------------------------------------------------------------
// Table of contents

pub struct Toc {
    /// `(offset, size)` of every section in logical order, offsets relative to the first byte
    /// after the TOC.
    pub sections: Vec<(usize, usize)>,
    /// Byte offset of the first section within the frame's data.
    pub data_start: usize,
}

impl Toc {
    pub fn parse(br: &mut Br, h: &FrameHeader) -> Result<Self> {
        let num_groups = h.num_groups();
        let entries = if num_groups == 1 && h.passes.num_passes == 1 {
            1
        } else {
            1 + h.num_lf_groups() as usize + 1 + num_groups as usize * h.passes.num_passes as usize
        };
        if entries > 65536 {
            return Err(bad("too many TOC entries"));
        }
        let permutation = if br.bool()? {
            let mut dec = Decoder::parse(br, 8)?;
            dec.begin(br)?;
            let p = read_permutation(br, &mut dec, entries as u32, 0)?;
            dec.finalize()?;
            Some(p)
        } else {
            None
        };
        br.zero_pad()?;
        let mut sizes = Vec::with_capacity(entries);
        for _ in 0..entries {
            sizes.push(br.u32([(0, 10), (1024, 14), (17408, 22), (4_211_712, 30)])? as usize);
        }
        br.zero_pad()?;
        let mut offsets = Vec::with_capacity(entries);
        let mut acc = 0usize;
        for &s in &sizes {
            offsets.push(acc);
            acc = acc.checked_add(s).ok_or_else(|| bad("TOC overflow"))?;
        }
        let sections = match permutation {
            Some(p) => p.iter().map(|&i| (offsets[i], sizes[i])).collect(),
            None => offsets.into_iter().zip(sizes).collect(),
        };
        Ok(Self {
            sections,
            data_start: br.bits_read() / 8,
        })
    }
}
