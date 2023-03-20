use anyhow::{anyhow, Result};
use image::codecs::avif::AvifEncoder;
use image::ImageEncoder;

fn main() -> Result<()> {
    let usage = "usage: input-file.png output-file.avif";
    let input = std::env::args().nth(1).ok_or_else(|| anyhow!(usage))?;
    let output = std::env::args().nth(2).ok_or_else(|| anyhow!(usage))?;

    let img = image::open(input)?;
    let mut out = tempfile_fast::Sponge::new_for(output)?;
    let enc = AvifEncoder::new_with_speed_quality(&mut out, 8, 70);
    enc.write_image(img.as_bytes(), img.width(), img.height(), img.color())?;
    out.commit()?;
    Ok(())
}
