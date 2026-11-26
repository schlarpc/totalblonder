//! Prints what each syncframe of an AC-3 file contains, using the crate's strict parser: rate,
//! layout, dialnorm, and per block the exponent strategies, short blocks, bandwidth and bits.
//! Frames the parser does not support (coupling, delta bit allocation) are reported as such.
//!
//! ```text
//! cargo run --release -p tb-ac3 --example inspect -- file.ac3 [max frames]
//! ```
#![allow(clippy::print_stdout, clippy::print_stderr)]

use tb_ac3::parse::{parse_frame, parse_header};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: inspect <file.ac3> [max frames]");
        std::process::exit(2);
    };
    let max: usize = args
        .get(1)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(usize::MAX);
    let data = std::fs::read(path)?;
    let mut at = 0;
    let mut n = 0;
    while at + 6 <= data.len() && n < max {
        let h = parse_header(&data[at..])?;
        let len = h.syncinfo.frame_bytes();
        let frame = &data[at..(at + len).min(data.len())];
        print!(
            "frame {n}: {} kbit/s acmod {} lfe {} dialnorm {} ",
            h.syncinfo.kbps(),
            h.bsi.acmod,
            u8::from(h.bsi.lfeon),
            h.bsi.dialnorm
        );
        match parse_frame(frame) {
            Ok(f) => {
                println!(
                    "snr {}/{} aux {} bits",
                    f.snr.coarse, f.snr.fine, f.aux_bits
                );
                for (b, blk) in f.blocks.iter().enumerate() {
                    println!(
                        "  block {b}: bits {:5} (side {:4}) snr {:?} expstr {:?} blksw {:?} end {:?} skip {:?}",
                        blk.end_bit - blk.start_bit,
                        blk.mantissa_bit - blk.start_bit,
                        blk.snr.map(|s| (s.coarse, s.fine)),
                        blk.expstr,
                        blk.blksw.iter().map(|&s| u8::from(s)).collect::<Vec<_>>(),
                        blk.end,
                        blk.skip
                    );
                }
            }
            Err(e) => println!("not parsed: {e}"),
        }
        at += len;
        n += 1;
    }
    Ok(())
}
