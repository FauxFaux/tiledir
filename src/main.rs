use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use clap::Parser;
use image::codecs::avif::AvifEncoder;
use image::imageops::FilterType;
use image::DynamicImage;
use image::ImageEncoder;
use itertools::Itertools;
use log::{debug, info};
use rand::prelude::*;
use rayon::prelude::*;
use regex::Regex;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// directory full of input images
    input: PathBuf,

    /// avif speed; 0 (slowest) - 10 (fastest); meaning not clearly defined
    ///
    /// 6 -> ~1h30m, 8 -> ~30 minutes; 10 -> ~10 minutes
    ///
    /// 6 is the upstream default but I can't say I'd recommend waiting
    #[clap(short, long, default_value = "8", value_parser = clap::value_parser!(u8).range(0..=10))]
    speed: u8,

    /// avif quality; 0 (terrible) - 100 (uselessly huge)
    ///
    /// 90 is very good, 70 is good, 60 is okay
    #[clap(short, long, default_value = "70", value_parser = clap::value_parser!(u8).range(0..=100))]
    quality: u8,
}

#[derive(Debug, Clone, Copy)]
struct ImageOps {
    quality: u8,
    speed: u8,
}

fn main() -> Result<()> {
    pretty_env_logger::init_timed();
    let format = Regex::new(r".*_(-?\d+)_(-?\d+)\.")?;
    let args: Cli = Cli::parse();

    let img_ops = ImageOps {
        quality: args.quality,
        speed: args.speed,
    };

    let base_wh = 4096u32;
    let tile_wh = 256u32;

    let tile_per_base = base_wh / tile_wh; // 16

    info!("discovering files...");
    let mut bases = Vec::new();
    for entry in fs::read_dir(args.input)? {
        let entry = entry?;
        let path = entry.path();
        let Some(file_name) = path.file_name() else {
            continue;
        };
        let file_name = file_name
            .to_str()
            .ok_or_else(|| anyhow!("unrepresentable filename: {file_name:?}"))?;
        let Some(captures) = format.captures(file_name) else {
            continue;
        };
        let x = captures
            .get(1)
            .ok_or(anyhow!("missing capture group 1"))?
            .as_str()
            .parse::<i64>()?;
        let y = captures
            .get(2)
            .ok_or(anyhow!("missing capture group 2"))?
            .as_str()
            .parse::<i64>()?;
        bases.push((x, y, entry.path()));
    }

    // let lx = bases.iter().map(|(x, _, _)| *x).min().expect("non-empty");
    // let ly = bases.iter().map(|(_, y, _)| *y).min().expect("non-empty");
    // let rx = bases.iter().map(|(x, _, _)| *x).max().expect("non-empty");
    // let ry = bases.iter().map(|(_, y, _)| *y).max().expect("non-empty");
    let lx = -15;
    let ly = -15;
    let rx = 16;
    let ry = 16;

    let bw = u32::try_from(rx - lx)?;
    let bh = u32::try_from(ry - ly)?;

    info!("files available from {lx}x{ly} -> {rx}x{ry} ({bw}x{bh})");

    let base_lookup = bases
        .into_iter()
        .map(|(x, y, path)| ((x, y), path))
        .collect::<HashMap<_, _>>();

    let mut xys = (0..bw)
        .flat_map(|x| (0..bh).map(move |y| (x, y)))
        .collect_vec();

    // try not to process all the empty tiles at the same time
    // (note that rayon already has a weird execution order)
    xys.shuffle(&mut thread_rng());

    let shrunk_res = 256;

    info!(
        "loading {} (shrunk) images for lower zoom levels...",
        xys.len()
    );

    let complete = AtomicUsize::new(0);

    let shrunk = xys
        .par_iter()
        .map(|(x, y)| -> Result<Option<((u32, u32), DynamicImage)>> {
            let Some(base) = base_lookup.get(&(i64::from(*x) + lx, i64::from(*y) + ly)) else {
                return Ok(None);
            };

            let img = image::open(base).with_context(|| anyhow!("reading {base:?} for shrunk"))?;
            if is_entirely_transparent(&img) {
                return Ok(None);
            }

            Ok(Some((
                (*x, *y),
                img.resize(shrunk_res, shrunk_res, FilterType::Lanczos3),
            )))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<HashMap<_, _>>();

    assert_eq!(bw, bh);
    let mega_res = (bw + 1) * shrunk_res;

    let total_non_blank = shrunk.len();
    info!(
        "{total_non_blank} (shrunk) images are non-empty, compositing into a {mega_res}² image..."
    );

    let mut mega = DynamicImage::new_rgba8(mega_res, mega_res);
    for y in 0..bh {
        for x in 0..bw {
            let Some(img) = shrunk.get(&(x, y)) else {
                continue;
            };
            image::imageops::overlay(
                &mut mega,
                img,
                i64::from(x * shrunk_res),
                i64::from(y * shrunk_res),
            );
        }
    }

    drop(shrunk);

    assert_eq!(mega.width(), mega.height());

    info!("slicing mega image into initial zoom levels...");

    (0..=4).into_par_iter().try_for_each(|zoom| -> Result<()> {
        let mul = 2u32.pow(zoom);
        let crop_wh = mega.width() / mul;
        for y in 0..mul {
            for x in 0..mul {
                create_dir_and_save(
                    format!("out/{zoom}/{x}/{y}.avif"),
                    &mega
                        .crop_imm(x * crop_wh, y * crop_wh, crop_wh, crop_wh)
                        .resize(tile_wh, tile_wh, FilterType::Lanczos3),
                    &img_ops,
                )?;
            }
        }
        Ok(())
    })?;

    drop(mega);

    info!("chopping individual bases into the remaining zoom levels...");

    xys.par_iter().try_for_each(|(x, y)| -> Result<()> {
        let Some(base) = base_lookup.get(&(i64::from(*x) + lx, i64::from(*y) + ly)) else {
            return Ok(());
        };

        let img = image::open(base)
            .with_context(|| anyhow!("reading {base:?} for remaining"))?;
        if is_entirely_transparent(&img) {
            debug!("skipping entirely transparent image {base:?}");
            return Ok(());
        }
        let mut time_manip = 0;
        let mut time_save = 0;
        for neg_zoom in 0..=4 {
            let mul = 2u32.pow(neg_zoom);
            let tiles = tile_per_base / mul;
            let step = tile_wh * mul;
            // 8 means 2^8 = 256 tiles; (4096 / 256px/tile) = 16 tiles per screenshot
            // 256/16 = 16 screenshots; i.e. -8 -> 7
            let zoom = 9 - neg_zoom;
            for ty in 0..tiles {
                for tx in 0..tiles {
                    let dx = x * tiles + tx;
                    let dy = y * tiles + ty;
                    let dest = format!("out/{zoom}/{dx}/{dy}.avif");
                    if fs::metadata(&dest).is_ok() {
                        continue;
                    }

                    let start = Instant::now();
                    let crop = img.crop_imm(tx * step, ty * step, step, step);
                    if is_entirely_transparent(&crop) {
                        debug!("skipping transparent cropped tile at {x}x{y} -> {tx}x{ty}");
                        continue;
                    }
                    let crop = crop.resize(tile_wh, tile_wh, FilterType::Lanczos3);
                    time_manip += start.elapsed().as_nanos();
                    let start = Instant::now();
                    create_dir_and_save(dest, &crop, &img_ops)?;
                    time_save += start.elapsed().as_nanos();
                    debug!("saved {tx}x{ty} in {x}x{y} as {dx}x{dy}");
                }
            }
        }

        let time_manip = time_manip as f64 / 1e9;
        let time_save = time_save as f64 / 1e9;
        let complete = complete.fetch_add(1, Ordering::SeqCst) + 1;
        info!("processed {complete}/{total_non_blank}: {base:?} (manip {time_manip:.2}s, save {time_save:.2}s)");

        Ok(())
    })?;

    info!("all done");

    Ok(())
}

/// only works for 8-bit images
fn is_entirely_transparent(img: &DynamicImage) -> bool {
    img.as_rgba8()
        .map(|img| img.pixels().all(|p| p.0[3] == 0))
        .unwrap_or(false)
}

fn create_dir_and_save(
    path: impl AsRef<Path>,
    img: &DynamicImage,
    img_ops: &ImageOps,
) -> Result<()> {
    let path = path.as_ref();
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| anyhow!("expected directory in path name, not {path:?}"))?,
    )
    .with_context(|| anyhow!("creating directories for {path:?}"))?;
    let mut out = tempfile_fast::Sponge::new_for(path)?;
    let enc = AvifEncoder::new_with_speed_quality(&mut out, img_ops.speed, img_ops.quality);
    enc.write_image(img.as_bytes(), img.width(), img.height(), img.color())?;
    out.commit()?;
    Ok(())
}
