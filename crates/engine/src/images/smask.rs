use crate::error::{OxideError, Result};
use crate::images::decoder::{ImageDecoder, RawImage};
use crate::images::locator::ImageReference;
use crate::object::PdfObject;
use crate::reader::PdfReader;

#[derive(Debug, Clone)]
pub struct SmaskLoader;

impl SmaskLoader {
    /// Decode and combine a soft mask for the main image, if one is present.
    pub fn load_and_combine(
        main_image: &ImageReference,
        main_raw: RawImage,
        reader: &PdfReader,
    ) -> Result<Option<RawImage>> {
        let obj = match reader.get_object(main_image.object_number, main_image.generation_number) {
            Ok(object) => object,
            Err(_) => return Ok(None),
        };
        let dict = match &obj {
            PdfObject::Stream { dict, .. } => dict.clone(),
            _ => return Ok(None),
        };

        let smask_ref = match dict.get("SMask") {
            Some(PdfObject::Reference { number, generation }) => (*number, *generation),
            _ => return Ok(None),
        };

        let smask_image_ref = ImageReference {
            page_number: main_image.page_number,
            xobject_name: format!("{}_smask", main_image.xobject_name),
            object_number: smask_ref.0,
            generation_number: smask_ref.1,
            width: main_image.width,
            height: main_image.height,
            bits_per_component: 8,
            color_space: "DeviceGray".to_string(),
            filter: vec![],
            is_inline: false,
            is_mask: false,
            is_smask: true,
            inline_data: None,
        };

        let smask_raw = match ImageDecoder::decode(&smask_image_ref, reader) {
            Ok(raw) => raw,
            Err(err) => {
                log::warn!(
                    "SMask decode failed for {}: {}",
                    main_image.xobject_name,
                    err
                );
                return Ok(None);
            }
        };

        Self::combine_rgba(main_raw, smask_raw).map(Some)
    }

    /// Combine a main image with a grayscale alpha mask into RGBA.
    pub fn combine_rgba(main: RawImage, mask: RawImage) -> Result<RawImage> {
        if main.width != mask.width || main.height != mask.height {
            log::warn!(
                "SMask dimensions {}x{} don't match image {}x{}; ignoring SMask",
                mask.width,
                mask.height,
                main.width,
                main.height
            );
            return Ok(main);
        }

        let pixel_count = main.width as usize * main.height as usize;
        let rgb: Vec<u8> = match main.channels {
            1 => main.pixels.iter().flat_map(|&g| [g, g, g]).collect(),
            3 => main.pixels.clone(),
            4 => {
                log::warn!("SMask combine: unexpected 4-channel main image; using RGB channels");
                main.pixels
                    .chunks_exact(4)
                    .flat_map(|c| [c[0], c[1], c[2]])
                    .collect()
            }
            channels => {
                return Err(OxideError::MalformedPdf(format!(
                    "SMask combine: unsupported main image channel count {}",
                    channels
                )))
            }
        };

        let mut rgba = Vec::with_capacity(pixel_count * 4);
        for i in 0..pixel_count {
            let r = rgb.get(i * 3).copied().unwrap_or(0);
            let g = rgb.get(i * 3 + 1).copied().unwrap_or(0);
            let b = rgb.get(i * 3 + 2).copied().unwrap_or(0);
            let a = mask.pixels.get(i).copied().unwrap_or(255);
            rgba.push(r);
            rgba.push(g);
            rgba.push(b);
            rgba.push(a);
        }

        Ok(RawImage {
            width: main.width,
            height: main.height,
            channels: 4,
            bits_per_sample: 8,
            pixels: rgba,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_rgba_single_fully_transparent_pixel() {
        let main = RawImage {
            width: 1,
            height: 1,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![200, 100, 50],
        };
        let mask = RawImage {
            width: 1,
            height: 1,
            channels: 1,
            bits_per_sample: 8,
            pixels: vec![0],
        };
        let combined = SmaskLoader::combine_rgba(main, mask).unwrap();
        assert_eq!(combined.pixels, vec![200, 100, 50, 0]);
    }

    #[test]
    fn combine_rgba_preserves_pixel_order() {
        let main = RawImage {
            width: 3,
            height: 1,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![10, 20, 30, 40, 50, 60, 70, 80, 90],
        };
        let mask = RawImage {
            width: 3,
            height: 1,
            channels: 1,
            bits_per_sample: 8,
            pixels: vec![100, 150, 200],
        };
        let out = SmaskLoader::combine_rgba(main, mask).unwrap();
        assert_eq!(
            out.pixels,
            vec![10, 20, 30, 100, 40, 50, 60, 150, 70, 80, 90, 200]
        );
    }

    #[test]
    fn combine_rgba_mask_shorter_than_main_pads_with_255() {
        let main = RawImage {
            width: 2,
            height: 1,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![0, 0, 0, 50, 50, 50],
        };
        let mask = RawImage {
            width: 2,
            height: 1,
            channels: 1,
            bits_per_sample: 8,
            pixels: vec![128],
        };
        let out = SmaskLoader::combine_rgba(main, mask).unwrap();
        assert_eq!(out.pixels[3], 128);
        assert_eq!(out.pixels[7], 255);
    }
}
