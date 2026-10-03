//! A one-page PDF holding the flattened image (File ▸ Export ▸ PDF):
//! the page is the image's print size at its resolution, the pixels are
//! embedded losslessly (Flate) or as JPEG, tagged sRGB through an ICC-based
//! colour space, with transparency as a soft mask.

use std::io::Write;

use lumenply_tiles::Raster;

use crate::{linear_to_srgb, IoError, SRGB_ICC};

/// How the pixels go into the PDF.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfImage {
    /// Flate-compressed RGB: exact, larger.
    Lossless,
    /// JPEG at this quality (1-100): smaller, no transparency.
    Jpeg(u8),
}

fn flate(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).expect("writing to memory");
    e.finish().expect("writing to memory")
}

/// Encode `raster` as a one-page PDF at `ppi` pixels per inch.
pub fn encode_pdf(raster: &Raster, ppi: f32, image: PdfImage) -> Result<Vec<u8>, IoError> {
    let (w, h) = (raster.width, raster.height);
    if w == 0 || h == 0 {
        return Err(IoError::Codec("the image is empty".into()));
    }
    let ppi = if ppi.is_finite() && ppi > 0.0 { ppi } else { 72.0 };
    // Points are 1/72 in.
    let (pw, ph) = (w as f32 * 72.0 / ppi, h as f32 * 72.0 / ppi);

    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    let mut alpha = Vec::with_capacity((w * h) as usize);
    let mut translucent = false;
    for p in &raster.pixels {
        let [r, g, b, a] = p.to_straight();
        rgb.extend_from_slice(&[linear_to_srgb(r), linear_to_srgb(g), linear_to_srgb(b)]);
        let a8 = (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        translucent |= a8 < 255;
        alpha.push(a8);
    }
    let (image_filter, image_data) = match image {
        PdfImage::Lossless => ("/FlateDecode", flate(&rgb)),
        PdfImage::Jpeg(q) => {
            // JPEG has no alpha: flatten onto white like the JPEG export.
            let jpeg = crate::encode_jpeg(raster, q)?;
            translucent = false;
            ("/DCTDecode", jpeg)
        }
    };

    // Objects: 1 catalog, 2 pages, 3 page, 4 image, 5 contents, 6 ICC
    // profile, 7 soft mask (when there is transparency).
    let mut objects: Vec<Vec<u8>> = Vec::new();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    objects.push(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec());
    objects.push(
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.3} {ph:.3}] \
             /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>"
        )
        .into_bytes(),
    );
    let smask = if translucent { " /SMask 7 0 R" } else { "" };
    let mut img = format!(
        "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace [/ICCBased 6 0 R] \
         /BitsPerComponent 8 /Filter {image_filter}{smask} /Length {} >>\nstream\n",
        image_data.len()
    )
    .into_bytes();
    img.extend_from_slice(&image_data);
    img.extend_from_slice(b"\nendstream");
    objects.push(img);
    let draw = format!("q {pw:.3} 0 0 {ph:.3} 0 0 cm /Im0 Do Q");
    objects.push(format!("<< /Length {} >>\nstream\n{draw}\nendstream", draw.len()).into_bytes());
    let icc = flate(SRGB_ICC);
    let mut prof = format!(
        "<< /N 3 /Alternate /DeviceRGB /Filter /FlateDecode /Length {} >>\nstream\n",
        icc.len()
    )
    .into_bytes();
    prof.extend_from_slice(&icc);
    prof.extend_from_slice(b"\nendstream");
    objects.push(prof);
    if translucent {
        let a = flate(&alpha);
        let mut m = format!(
            "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceGray \
             /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>\nstream\n",
            a.len()
        )
        .into_bytes();
        m.extend_from_slice(&a);
        m.extend_from_slice(b"\nendstream");
        objects.push(m);
    }

    let mut out = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info << /Producer (Lumenply) >> >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_tiles::Rgba;

    fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }

    #[test]
    fn a_pdf_page_has_the_print_size_and_exact_pixels() {
        let mut r = Raster::new(600, 300);
        for (i, p) in r.pixels.iter_mut().enumerate() {
            *p = if i % 600 < 300 {
                Rgba::from_straight(1.0, 0.0, 0.0, 1.0)
            } else {
                Rgba::from_straight(0.0, 0.0, 1.0, 0.5)
            };
        }
        let pdf = encode_pdf(&r, 300.0, PdfImage::Lossless).unwrap();
        // For checking with independent readers (qpdf, pdfinfo, PyMuPDF).
        if let Ok(dir) = std::env::var("LUMENPLY_KEEP_PDF") {
            let _ = std::fs::write(format!("{dir}/test.pdf"), &pdf);
        }
        assert!(pdf.starts_with(b"%PDF-1.4"));
        // 600 × 300 px at 300 ppi = 2 × 1 in = 144 × 72 pt.
        assert!(find(&pdf, b"/MediaBox [0 0 144.000 72.000]").is_some());
        assert!(
            find(&pdf, b"/SMask 7 0 R").is_some(),
            "half-clear pixels need a mask"
        );
        // Every xref offset points at its object.
        let x = find(&pdf, b"xref\n").unwrap();
        let table = std::str::from_utf8(&pdf[x..]).unwrap();
        for (n, line) in table.lines().skip(3).take(7).enumerate() {
            let at: usize = line[..10].parse().unwrap();
            assert!(
                pdf[at..].starts_with(format!("{} 0 obj", n + 1).as_bytes()),
                "object {}",
                n + 1
            );
        }
        // The image stream inflates back to the exact sRGB bytes.
        let s = find(&pdf, b"/Subtype /Image /Width 600").unwrap();
        let start = s + find(&pdf[s..], b"stream\n").unwrap() + 7;
        let end = start + find(&pdf[start..], b"\nendstream").unwrap();
        let mut d = flate2::read::ZlibDecoder::new(&pdf[start..end]);
        let mut rgb = Vec::new();
        std::io::Read::read_to_end(&mut d, &mut rgb).unwrap();
        assert_eq!(rgb.len(), 600 * 300 * 3);
        assert_eq!(&rgb[..3], &[255, 0, 0]);
        assert_eq!(&rgb[3 * 599..3 * 600], &[0, 0, 255]);
        // JPEG: no mask, and the DCT stream is a JPEG.
        let jpeg = encode_pdf(&r, 72.0, PdfImage::Jpeg(85)).unwrap();
        assert!(find(&jpeg, b"/DCTDecode").is_some() && find(&jpeg, b"/SMask").is_none());
        assert!(find(&jpeg, b"/MediaBox [0 0 600.000 300.000]").is_some());
        assert!(find(&jpeg, &[0xFF, 0xD8, 0xFF]).is_some());
    }
}
