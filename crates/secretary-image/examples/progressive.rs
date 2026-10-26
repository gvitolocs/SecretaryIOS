//! `progressive <file.jxl>...`: feed each file in 8 KB chunks (as the apps do) and report after
//! how many bytes the first picture and the complete one appear, and how many refinements.
use std::time::Instant;

fn main() {
    let (mut first_frac, mut n, mut passes_sum, mut ms_sum) = (0.0, 0usize, 0usize, 0.0);
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let mut d = secretary_image::Decoder::new();
        d.render_step = 8 * 1024;
        let (mut first, mut passes) = (None, 0);
        let t = Instant::now();
        let chunks: Vec<&[u8]> = bytes.chunks(8 * 1024).collect();
        for (i, c) in chunks.iter().enumerate() {
            if d.feed(c, i + 1 == chunks.len()).unwrap() {
                passes += 1;
                first.get_or_insert((i + 1) * 8 * 1024);
            }
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let f = first.unwrap_or(bytes.len()).min(bytes.len()) as f64 / bytes.len() as f64;
        println!("{path}: {} KB, first picture after {:.0}% of the bytes, {passes} pictures, {ms:.0} ms decoding", bytes.len() / 1024, f * 100.0);
        first_frac += f;
        passes_sum += passes;
        ms_sum += ms;
        n += 1;
    }
    if n > 0 {
        println!("average over {n}: first picture after {:.0}% of the bytes, {:.1} pictures, {:.0} ms decoding", first_frac / n as f64 * 100.0, passes_sum as f64 / n as f64, ms_sum / n as f64);
    }
}
