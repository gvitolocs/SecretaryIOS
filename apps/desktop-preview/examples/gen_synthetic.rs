//! Deterministic synthetic novel for benchmarks: `cargo run --example gen_synthetic -- out.txt 2000000`.
//!
//! English-like prose from a fixed vocabulary (pseudo-random, seeded), paragraphs separated by blank lines
//! and a `CHAPTER N` heading every ~60 paragraphs, so it exercises the TXT importer, chapter detection,
//! justification and pagination like a public-domain novel would, without shipping one.
use std::io::Write;

const WORDS: &[&str] = &[
    "the",
    "of",
    "and",
    "a",
    "to",
    "in",
    "he",
    "was",
    "that",
    "it",
    "his",
    "her",
    "with",
    "as",
    "had",
    "for",
    "she",
    "at",
    "on",
    "not",
    "but",
    "be",
    "by",
    "which",
    "they",
    "from",
    "have",
    "were",
    "all",
    "said",
    "one",
    "this",
    "there",
    "would",
    "been",
    "upon",
    "their",
    "could",
    "little",
    "very",
    "old",
    "house",
    "morning",
    "letter",
    "garden",
    "evening",
    "window",
    "river",
    "carriage",
    "silence",
    "remembered",
    "presently",
    "neighbourhood",
    "extraordinary",
    "conversation",
    "understanding",
    "gentleman",
    "melancholy",
    "candle",
    "staircase",
    "beneath",
    "whispered",
    "honest",
    "quiet",
    "afterwards",
    "certainly",
    "perhaps",
    "between",
    "himself",
    "herself",
    "moment",
    "nothing",
    "everything",
    "something",
    "nobody",
    "children",
    "country",
    "village",
    "winter",
    "summer",
    "light",
    "darkness",
    "door",
    "table",
    "chair",
    "fire",
    "bread",
    "coffee",
    "road",
    "hill",
    "field",
    "sea",
    "ship",
    "storm",
    "rain",
    "wind",
    "voice",
    "face",
    "hand",
    "eyes",
    "heart",
    "mind",
    "thought",
    "walked",
    "looked",
    "turned",
    "answered",
    "replied",
    "asked",
    "smiled",
    "waited",
    "returned",
    "slowly",
    "quickly",
    "suddenly",
    "again",
    "never",
    "always",
    "still",
    "already",
    "almost",
    "only",
    "great",
    "small",
    "young",
    "long",
    "dark",
    "bright",
    "strange",
    "kind",
    "proud",
    "tired",
    "careful",
    "Mr.",
    "Mrs.",
    "Elinor",
    "Thomas",
    "Margaret",
    "Captain",
    "Doctor",
    "London",
    "Thursday",
    "o'clock",
];

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().expect("usage: gen_synthetic OUT.txt BYTES");
    let target: usize = args.next().map_or(2_000_000, |b| b.parse().expect("BYTES"));
    let mut rng = Lcg(0x5eed_1ab5);
    let mut s = String::with_capacity(target + 4096);
    let mut chapter = 0;
    let mut para_in_chapter = 60;
    while s.len() < target {
        if para_in_chapter >= 60 {
            chapter += 1;
            para_in_chapter = 0;
            s.push_str(&format!("CHAPTER {chapter}\n\n"));
        }
        let sentences = rng.range(2, 7);
        let mut para = String::new();
        for _ in 0..sentences {
            let n = rng.range(6, 24);
            for i in 0..n {
                let w = WORDS[rng.next() as usize % WORDS.len()];
                if i == 0 {
                    let mut c = w.chars();
                    let first = c.next().unwrap_or('A').to_uppercase().collect::<String>();
                    para.push_str(&first);
                    para.push_str(c.as_str());
                } else {
                    para.push(' ');
                    para.push_str(w);
                }
                if i + 1 < n && rng.range(0, 9) == 0 {
                    para.push(',');
                }
            }
            para.push_str(if rng.range(0, 7) == 0 { "? " } else { ". " });
        }
        // wrap at ~70 columns like a Gutenberg text file
        let mut col = 0;
        for w in para.trim_end().split(' ') {
            if col + w.len() > 70 {
                s.push('\n');
                col = 0;
            } else if col > 0 {
                s.push(' ');
                col += 1;
            }
            s.push_str(w);
            col += w.len();
        }
        s.push_str("\n\n");
        para_in_chapter += 1;
    }
    std::fs::File::create(&out)
        .and_then(|mut f| f.write_all(s.as_bytes()))
        .expect("write output");
    eprintln!("{out}: {} bytes, {chapter} chapters", s.len());
}
