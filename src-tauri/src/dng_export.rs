use image::{ImageBuffer, Rgb};
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

pub type ImageRgb16 = ImageBuffer<Rgb<u16>, Vec<u16>>;

// TIFF field types
const TIFF_BYTE: u16 = 1;
const TIFF_ASCII: u16 = 2;
const TIFF_SHORT: u16 = 3;
const TIFF_LONG: u16 = 4;
const TIFF_RATIONAL: u16 = 5;
const TIFF_SRATIONAL: u16 = 10;

// TIFF tags
const TAG_NEW_SUBFILE_TYPE: u16 = 254;
const TAG_IMAGE_WIDTH: u16 = 256;
const TAG_IMAGE_LENGTH: u16 = 257;
const TAG_BITS_PER_SAMPLE: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
const TAG_PHOTOMETRIC_INTERPRETATION: u16 = 262;
const TAG_MAKE: u16 = 271;
const TAG_MODEL: u16 = 272;
const TAG_STRIP_OFFSETS: u16 = 273;
const TAG_SAMPLES_PER_PIXEL: u16 = 277;
const TAG_ROWS_PER_STRIP: u16 = 278;
const TAG_STRIP_BYTE_COUNTS: u16 = 279;
const TAG_PLANAR_CONFIGURATION: u16 = 284;
const TAG_SAMPLE_FORMAT: u16 = 339;

// CFA tags
const TAG_CFA_REPEAT_PATTERN_DIM: u16 = 33421;
const TAG_CFA_PATTERN: u16 = 33422;

// DNG tags
const TAG_DNG_VERSION: u16 = 50706;
const TAG_DNG_BACKWARD_VERSION: u16 = 50707;
const TAG_UNIQUE_CAMERA_MODEL: u16 = 50708;
const TAG_CFA_PLANE_COLOR: u16 = 50710;
const TAG_CFA_LAYOUT: u16 = 50711;
const TAG_BLACK_LEVEL_REPEAT_DIM: u16 = 50713;
const TAG_BLACK_LEVEL: u16 = 50714;
const TAG_WHITE_LEVEL: u16 = 50717;
const TAG_DEFAULT_CROP_ORIGIN: u16 = 50719;
const TAG_DEFAULT_CROP_SIZE: u16 = 50720;
const TAG_COLOR_MATRIX_1: u16 = 50721;
const TAG_AS_SHOT_NEUTRAL: u16 = 50728;
const TAG_CALIBRATION_ILLUMINANT_1: u16 = 50778;
const TAG_FORWARD_MATRIX_1: u16 = 50964;

// TIFF/DNG values
const COMPRESSION_NONE: u16 = 1;
const PLANAR_CONFIGURATION_CHUNKY: u16 = 1;
const SAMPLE_FORMAT_UNSIGNED_INTEGER: u16 = 1;

const PHOTOMETRIC_CFA: u16 = 32803;
const PHOTOMETRIC_LINEAR_RAW: u16 = 34892;

// CalibrationIlluminant1 value for D65
const LIGHT_SOURCE_D65: u16 = 21;

#[derive(Clone, Debug)]
struct IfdEntry {
    tag: u16,
    field_type: u16,
    count: u32,
    data: Vec<u8>,
}

impl IfdEntry {
    fn new(tag: u16, field_type: u16, count: u32, data: Vec<u8>) -> Self {
        Self {
            tag,
            field_type,
            count,
            data,
        }
    }

    fn byte(tag: u16, values: &[u8]) -> Self {
        Self::new(tag, TIFF_BYTE, values.len() as u32, values.to_vec())
    }

    fn ascii(tag: u16, value: &str) -> Self {
        let mut data = value.as_bytes().to_vec();

        if !data.ends_with(&[0]) {
            data.push(0);
        }

        Self::new(tag, TIFF_ASCII, data.len() as u32, data)
    }

    fn short(tag: u16, values: &[u16]) -> Self {
        let mut data = Vec::with_capacity(values.len() * 2);

        for &value in values {
            data.extend_from_slice(&value.to_le_bytes());
        }

        Self::new(tag, TIFF_SHORT, values.len() as u32, data)
    }

    fn long(tag: u16, values: &[u32]) -> Self {
        let mut data = Vec::with_capacity(values.len() * 4);

        for &value in values {
            data.extend_from_slice(&value.to_le_bytes());
        }

        Self::new(tag, TIFF_LONG, values.len() as u32, data)
    }

    fn rational(tag: u16, values: &[(u32, u32)]) -> Self {
        let mut data = Vec::with_capacity(values.len() * 8);

        for &(numerator, denominator) in values {
            data.extend_from_slice(&numerator.to_le_bytes());
            data.extend_from_slice(&denominator.to_le_bytes());
        }

        Self::new(tag, TIFF_RATIONAL, values.len() as u32, data)
    }

    fn srational(tag: u16, values: &[(i32, i32)]) -> Self {
        let mut data = Vec::with_capacity(values.len() * 8);

        for &(numerator, denominator) in values {
            data.extend_from_slice(&numerator.to_le_bytes());
            data.extend_from_slice(&denominator.to_le_bytes());
        }

        Self::new(tag, TIFF_SRATIONAL, values.len() as u32, data)
    }
}

#[derive(Clone, Debug)]
enum DngLayout {
    Linear,
    Bayer {
        name: &'static str,
        pattern: [u8; 4],
    },
    XTrans,
}

impl DngLayout {
    fn parse(value: &str) -> io::Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "linear" => Ok(Self::Linear),

            "rggb" => Ok(Self::Bayer {
                name: "RGGB",
                pattern: [0, 1, 1, 2],
            }),

            "bggr" => Ok(Self::Bayer {
                name: "BGGR",
                pattern: [2, 1, 1, 0],
            }),

            "grbg" => Ok(Self::Bayer {
                name: "GRBG",
                pattern: [1, 0, 2, 1],
            }),

            "gbrg" => Ok(Self::Bayer {
                name: "GBRG",
                pattern: [1, 2, 0, 1],
            }),

            "xtrans" | "x-trans" => Ok(Self::XTrans),

            other => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Onbekend CFA-type '{other}'. \
                     Gebruik \"\", \"linear\", \"rggb\", \"bggr\", \
                     \"grbg\", \"gbrg\" of \"xtrans\"."
                ),
            )),
        }
    }

    fn is_linear(&self) -> bool {
        matches!(self, Self::Linear)
    }
}

const XTRANS_PATTERN: [[u8; 6]; 6] = [
    [1, 2, 0, 1, 0, 2],
    [0, 1, 1, 2, 1, 1],
    [2, 1, 1, 0, 1, 1],
    [1, 0, 2, 1, 2, 0],
    [2, 1, 1, 0, 1, 1],
    [0, 1, 1, 2, 1, 1],
];

fn simulate_bayer(image: &ImageRgb16, pattern: [u8; 4]) -> Vec<u16> {
    let width = image.width() as usize;
    let height = image.height() as usize;

    let mut output = Vec::with_capacity(width * height);

    for y in 0..height {
        for x in 0..width {
            let pattern_index = (y % 2) * 2 + (x % 2);
            let channel = pattern[pattern_index] as usize;
            let pixel = image.get_pixel(x as u32, y as u32);

            output.push(pixel[channel]);
        }
    }

    output
}

fn simulate_xtrans(image: &ImageRgb16) -> Vec<u16> {
    let width = image.width() as usize;
    let height = image.height() as usize;

    let mut output = Vec::with_capacity(width * height);

    for y in 0..height {
        for x in 0..width {
            let channel = XTRANS_PATTERN[y % 6][x % 6] as usize;
            let pixel = image.get_pixel(x as u32, y as u32);

            output.push(pixel[channel]);
        }
    }

    output
}

fn linear_rgb_data(image: &ImageRgb16) -> Vec<u16> {
    image.as_raw().clone()
}

fn u16_slice_to_le_bytes(values: &[u16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 2);

    for &value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    bytes
}

fn matrix_to_srational(
    matrix: [[f64; 3]; 3],
    denominator: i32,
) -> [(i32, i32); 9] {
    let mut output = [(0, denominator); 9];
    let mut index = 0;

    for row in matrix {
        for value in row {
            output[index] = (
                (value * denominator as f64).round() as i32,
                denominator,
            );

            index += 1;
        }
    }

    output
}

fn neutral_to_rational(
    neutral: [f64; 3],
    denominator: u32,
) -> [(u32, u32); 3] {
    neutral.map(|value| {
        let value = value.max(0.0);

        (
            (value * denominator as f64).round() as u32,
            denominator,
        )
    })
}

fn align_even(value: u32) -> u32 {
    (value + 1) & !1
}

fn write_padding<W: Write>(
    writer: &mut W,
    count: usize,
) -> io::Result<()> {
    const ZEROES: [u8; 8] = [0; 8];

    let mut remaining = count;

    while remaining > 0 {
        let amount = remaining.min(ZEROES.len());
        writer.write_all(&ZEROES[..amount])?;
        remaining -= amount;
    }

    Ok(())
}

fn write_ifd_entry<W: Write>(
    writer: &mut W,
    entry: &IfdEntry,
    data_offset: u32,
) -> io::Result<()> {
    writer.write_all(&entry.tag.to_le_bytes())?;
    writer.write_all(&entry.field_type.to_le_bytes())?;
    writer.write_all(&entry.count.to_le_bytes())?;

    if entry.data.len() <= 4 {
        writer.write_all(&entry.data)?;
        write_padding(writer, 4 - entry.data.len())?;
    } else {
        writer.write_all(&data_offset.to_le_bytes())?;
    }

    Ok(())
}

/// Schrijft een Linear DNG of gesimuleerde CFA-DNG.
///
/// `cfa_type`:
/// - `""` of `"linear"`: LinearRaw RGB16
/// - `"rggb"`, `"bggr"`, `"grbg"`, `"gbrg"`: Bayer CFA
/// - `"xtrans"`: 6x6 X-Trans CFA
///
/// `color_matrix_1` en `forward_matrix_1` worden als SRATIONAL
/// met noemer 10000 geschreven.
///
/// `as_shot_neutral` bevat de neutrale RGB-verhoudingen, bijvoorbeeld:
/// `[0.52, 1.0, 0.72]`.
pub fn save_dng<P: AsRef<Path>>(
    image: &ImageRgb16,
    path: P,
    cfa_type: &str,
    color_matrix_1: [[f64; 3]; 3],
    forward_matrix_1: Option<[[f64; 3]; 3]>,
    as_shot_neutral: [f64; 3],
) -> io::Result<()> {
    if image.width() == 0 || image.height() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "De afbeelding heeft een breedte of hoogte van nul.",
        ));
    }

    let layout = DngLayout::parse(cfa_type)?;

    let width = image.width();
    let height = image.height();

    let pixel_values = match &layout {
        DngLayout::Linear => linear_rgb_data(image),

        DngLayout::Bayer { pattern, .. } => {
            simulate_bayer(image, *pattern)
        }

        DngLayout::XTrans => simulate_xtrans(image),
    };

    let pixel_bytes = u16_slice_to_le_bytes(&pixel_values);

    if pixel_bytes.len() > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Pixeldata is te groot voor klassieke TIFF/DNG-offsets.",
        ));
    }

    let samples_per_pixel: u16 = if layout.is_linear() { 3 } else { 1 };

    let photometric = if layout.is_linear() {
        PHOTOMETRIC_LINEAR_RAW
    } else {
        PHOTOMETRIC_CFA
    };

    let color_matrix = matrix_to_srational(color_matrix_1, 10_000);

    // let forward_matrix = matrix_to_srational(
    //     forward_matrix_1.unwrap_or(color_matrix_1),
    //     10_000,
    // );

    let white_balance = neutral_to_rational(as_shot_neutral, 10_000);

    let mut entries = vec![
        IfdEntry::long(TAG_NEW_SUBFILE_TYPE, &[0]),
        IfdEntry::long(TAG_IMAGE_WIDTH, &[width]),
        IfdEntry::long(TAG_IMAGE_LENGTH, &[height]),
        IfdEntry::short(
            TAG_BITS_PER_SAMPLE,
            if layout.is_linear() {
                &[16, 16, 16]
            } else {
                &[16]
            },
        ),
        IfdEntry::short(TAG_COMPRESSION, &[COMPRESSION_NONE]),
        IfdEntry::short(
            TAG_PHOTOMETRIC_INTERPRETATION,
            &[photometric],
        ),
        IfdEntry::ascii(TAG_MAKE, "Custom"),
        IfdEntry::ascii(TAG_MODEL, "Synthetic Camera"),

        // StripOffsets wordt later ingevuld.
        IfdEntry::long(TAG_STRIP_OFFSETS, &[0]),

        IfdEntry::short(
            TAG_SAMPLES_PER_PIXEL,
            &[samples_per_pixel],
        ),
        IfdEntry::long(TAG_ROWS_PER_STRIP, &[height]),
        IfdEntry::long(
            TAG_STRIP_BYTE_COUNTS,
            &[pixel_bytes.len() as u32],
        ),
        IfdEntry::short(
            TAG_PLANAR_CONFIGURATION,
            &[PLANAR_CONFIGURATION_CHUNKY],
        ),
        IfdEntry::short(
            TAG_SAMPLE_FORMAT,
            if layout.is_linear() {
                &[
                    SAMPLE_FORMAT_UNSIGNED_INTEGER,
                    SAMPLE_FORMAT_UNSIGNED_INTEGER,
                    SAMPLE_FORMAT_UNSIGNED_INTEGER,
                ]
            } else {
                &[SAMPLE_FORMAT_UNSIGNED_INTEGER]
            },
        ),

        IfdEntry::byte(TAG_DNG_VERSION, &[1, 4, 0, 0]),
        IfdEntry::byte(
            TAG_DNG_BACKWARD_VERSION,
            &[1, 1, 0, 0],
        ),
        IfdEntry::ascii(
            TAG_UNIQUE_CAMERA_MODEL,
            "Synthetic Camera",
        ),

        IfdEntry::srational(
            TAG_COLOR_MATRIX_1,
            &color_matrix,
        ),
/*        IfdEntry::srational(
            TAG_FORWARD_MATRIX_1,
            &forward_matrix,
        ), */
        IfdEntry::rational(
            TAG_AS_SHOT_NEUTRAL,
            &white_balance,
        ),
        IfdEntry::short(
            TAG_CALIBRATION_ILLUMINANT_1,
            &[LIGHT_SOURCE_D65],
        ),

        IfdEntry::long(TAG_WHITE_LEVEL, &[65535]),
        IfdEntry::long(TAG_DEFAULT_CROP_ORIGIN, &[0, 0]),
        IfdEntry::long(
            TAG_DEFAULT_CROP_SIZE,
            &[width, height],
        ),
    ];

    match &layout {
        DngLayout::Linear => {
            entries.push(IfdEntry::rational(
                TAG_BLACK_LEVEL,
                &[(0, 1), (0, 1), (0, 1)],
            ));
        }

        DngLayout::Bayer { name: _, pattern } => {
            entries.push(IfdEntry::short(
                TAG_CFA_REPEAT_PATTERN_DIM,
                &[2, 2],
            ));

            entries.push(IfdEntry::byte(
                TAG_CFA_PATTERN,
                pattern,
            ));

            entries.push(IfdEntry::byte(
                TAG_CFA_PLANE_COLOR,
                &[0, 1, 2],
            ));

            entries.push(IfdEntry::short(
                TAG_CFA_LAYOUT,
                &[1],
            ));

            entries.push(IfdEntry::short(
                TAG_BLACK_LEVEL_REPEAT_DIM,
                &[2, 2],
            ));

            entries.push(IfdEntry::rational(
                TAG_BLACK_LEVEL,
                &[(0, 1), (0, 1), (0, 1), (0, 1)],
            ));
        }

        DngLayout::XTrans => {
            let pattern: Vec<u8> = XTRANS_PATTERN
                .iter()
                .flat_map(|row| row.iter().copied())
                .collect();

            entries.push(IfdEntry::short(
                TAG_CFA_REPEAT_PATTERN_DIM,
                &[6, 6],
            ));

            entries.push(IfdEntry::byte(
                TAG_CFA_PATTERN,
                &pattern,
            ));

            entries.push(IfdEntry::byte(
                TAG_CFA_PLANE_COLOR,
                &[0, 1, 2],
            ));

            entries.push(IfdEntry::short(
                TAG_CFA_LAYOUT,
                &[1],
            ));

            entries.push(IfdEntry::short(
                TAG_BLACK_LEVEL_REPEAT_DIM,
                &[6, 6],
            ));

            let black_levels = vec![(0u32, 1u32); 36];

            entries.push(IfdEntry::rational(
                TAG_BLACK_LEVEL,
                &black_levels,
            ));
        }
    }

    entries.sort_by_key(|entry| entry.tag);

    let entry_count = entries.len();

    if entry_count > u16::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Te veel IFD-entries.",
        ));
    }

    // TIFF-header:
    // 2 bytes byte order
    // 2 bytes TIFF magic
    // 4 bytes offset naar eerste IFD
    const TIFF_HEADER_SIZE: u32 = 8;
    const FIRST_IFD_OFFSET: u32 = TIFF_HEADER_SIZE;

    // IFD:
    // 2 bytes aantal entries
    // 12 bytes per entry
    // 4 bytes offset naar volgende IFD
    let ifd_size =
        2u32 + (entry_count as u32 * 12u32) + 4u32;

    let mut external_data_offset =
        align_even(FIRST_IFD_OFFSET + ifd_size);

    let mut data_offsets = vec![0u32; entry_count];

    for (index, entry) in entries.iter().enumerate() {
        if entry.data.len() > 4 {
            data_offsets[index] = external_data_offset;

            external_data_offset = align_even(
                external_data_offset
                    .checked_add(entry.data.len() as u32)
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "DNG-offset overflow.",
                        )
                    })?,
            );
        }
    }

    let strip_offset = align_even(external_data_offset);

    for entry in &mut entries {
        if entry.tag == TAG_STRIP_OFFSETS {
            *entry = IfdEntry::long(
                TAG_STRIP_OFFSETS,
                &[strip_offset],
            );
            break;
        }
    }

    let mut file = File::create(path)?;

    // Little-endian TIFF-header.
    file.write_all(b"II")?;
    file.write_all(&42u16.to_le_bytes())?;
    file.write_all(&FIRST_IFD_OFFSET.to_le_bytes())?;

    // Eerste en enige IFD.
    file.seek(SeekFrom::Start(FIRST_IFD_OFFSET as u64))?;
    file.write_all(&(entry_count as u16).to_le_bytes())?;

    for (index, entry) in entries.iter().enumerate() {
        write_ifd_entry(
            &mut file,
            entry,
            data_offsets[index],
        )?;
    }

    // Geen volgende IFD.
    file.write_all(&0u32.to_le_bytes())?;

    // Schrijf alle waarden die niet in de 4-byte value/offset-ruimte passen.
    for (index, entry) in entries.iter().enumerate() {
        if entry.data.len() <= 4 {
            continue;
        }

        file.seek(SeekFrom::Start(data_offsets[index] as u64))?;
        file.write_all(&entry.data)?;

        if entry.data.len() % 2 != 0 {
            file.write_all(&[0])?;
        }
    }

    // Ongecomprimeerde pixelstrip.
    file.seek(SeekFrom::Start(strip_offset as u64))?;
    file.write_all(&pixel_bytes)?;
    file.flush()?;

    Ok(())
}

fn create_test_image(width: u32, height: u32) -> ImageRgb16 {
    ImageBuffer::from_fn(width, height, |x, y| {
        let r = if width > 1 {
            ((x as u64 * 65535) / (width - 1) as u64) as u16
        } else {
            0
        };

        let g = if height > 1 {
            ((y as u64 * 65535) / (height - 1) as u64) as u16
        } else {
            0
        };

        let denominator = (width as u64 + height as u64)
            .saturating_sub(2)
            .max(1);

        let b = (
            ((x as u64 + y as u64) * 65535) / denominator
        ) as u16;

        Rgb([r, g, b])
    })
}

fn main() -> io::Result<()> {
    let image = create_test_image(1024, 768);

    // Voorbeeldmatrix. Vervang deze door de CCM uit je RAW metadata.
    let color_matrix_1 = [
        [1.0000, 0.0000, 0.0000],
        [0.0000, 1.0000, 0.0000],
        [0.0000, 0.0000, 1.0000],
    ];

    // In je Python-code wordt ForwardMatrix1 gelijkgesteld
    // aan ColorMatrix1. `None` doet hier hetzelfde.
    let forward_matrix_1 = None;

    // Vergelijkbaar met:
    // R-neutral = green_multiplier / red_multiplier
    // G-neutral = 1
    // B-neutral = green_multiplier / blue_multiplier
    let as_shot_neutral = [1.0, 1.0, 1.0];

    // Linear DNG:
    save_dng(
        &image,
        "output-linear.dng",
        "",
        color_matrix_1,
        forward_matrix_1,
        as_shot_neutral,
    )?;

    // Gesimuleerde Bayer RGGB-DNG:
    save_dng(
        &image,
        "output-rggb.dng",
        "rggb",
        color_matrix_1,
        forward_matrix_1,
        as_shot_neutral,
    )?;

    // Gesimuleerde X-Trans-DNG:
    save_dng(
        &image,
        "output-xtrans.dng",
        "xtrans",
        color_matrix_1,
        forward_matrix_1,
        as_shot_neutral,
    )?;

    println!("DNG-bestanden geschreven.");

    Ok(())
}