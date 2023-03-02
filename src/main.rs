use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;
use image::codecs::avif::AvifEncoder;
use itertools::Itertools;
use log::{debug, info};
use rayon::prelude::*;
use regex::Regex;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// directory full of input images
    input: PathBuf,
}

fn main() -> Result<()> {
    pretty_env_logger::init();
    let format = Regex::new(r".*_(-?\d+)_(-?\d+)\.")?;
    let args: Cli = Cli::parse();

    let base_wh = (4096, 4096u32);
    let tile_wh = (256, 256);

    let tile_per_base = base_wh.0 / tile_wh.0; // 16

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
        let Some(captures) = format.captures(file_name) else { continue; };
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

    let lx = bases.iter().map(|(x, _, _)| *x).min().expect("non-empty");
    let ly = bases.iter().map(|(_, y, _)| *y).min().expect("non-empty");
    let rx = bases.iter().map(|(x, _, _)| *x).max().expect("non-empty");
    let ry = bases.iter().map(|(_, y, _)| *y).max().expect("non-empty");

    let bw = u32::try_from(rx - lx)?;
    let bh = u32::try_from(ry - ly)?;

    let base_lookup = bases
        .into_iter()
        .map(|(x, y, path)| ((x, y), path))
        .collect::<HashMap<_, _>>();

    let xys = (0..bw)
        .flat_map(|x| (0..bh).map(move |y| (x, y)))
        .collect_vec();

    xys.into_par_iter().try_for_each(|(x, y)| -> Result<()> {
        let Some(base) = base_lookup.get(&(i64::from(x) + lx, i64::from(y) + ly)) else {
            return Ok(());
        };

        let img = image::open(&base)?;
        info!("loaded {base:?}");
        for ty in 0..tile_per_base {
            for tx in 0..tile_per_base {
                let crop = img.crop_imm(tx * tile_wh.0, ty * tile_wh.1, tile_wh.0, tile_wh.1);
                let dx = x * tile_per_base + tx;
                let dy = y * tile_per_base + ty;
                create_dir_and_save(format!("out/4/{dx}/{dy}.avif"), &crop)?;
                debug!("saved {tx}x{ty} in {x}x{y} as {dx}x{dy}");
            }
        }

        Ok(())
    })?;

    // for (x, y, path) in bases {
    //     let img = image::open(&path)?;
    //     let t00 = img.crop_imm(0, 0, 256, 256);
    //     img.resize(256, 256, image::imageops::FilterType::Nearest);
    //     println!("{} {} {} {} {:?}", x, y, img.width(), img.height(), path);
    // }
    Ok(())
}

fn create_dir_and_save(path: impl AsRef<Path>, img: &image::DynamicImage) -> Result<()> {
    let path = path.as_ref();
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| anyhow!("expected directory in path name, not {path:?}"))?,
    )
    .with_context(|| anyhow!("creating directories for {path:?}"))?;
    let mut out = tempfile_fast::Sponge::new_for(path)?;
    let enc = AvifEncoder::new_with_speed_quality(&mut out, 10, 70);
    enc.write_image(img.as_bytes(), img.width(), img.height(), img.color())?;
    out.commit()?;
    // img.save_with_format(path, ImageFormat::Avif)?;
    Ok(())
}
