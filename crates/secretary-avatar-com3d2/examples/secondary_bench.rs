//! Reproducible CPU-only benchmark; no rendering, asset IO or browser bridge.
use glam::{Mat4, Vec3};
use secretary_avatar_com3d2::secondary_physics::{Capsule, Input, Spring};
fn main() {
    for count in [22usize, 128] {
        let mut states = vec![Spring::new(); count];
        let colliders = [Capsule {
            a: Vec3::new(0., 0., 0.),
            b: Vec3::new(0., 0.5, 0.),
            radius: 0.1,
        }; 8];
        let mut times = Vec::new();
        for frame in 0..1300 {
            let begin = std::time::Instant::now();
            for (i, s) in states.iter_mut().enumerate() {
                let x = i as f32 * 0.003 + 0.11;
                let root = Vec3::new(x, 0.5, (frame as f32 * 0.01).sin() * 0.01);
                let input = Input {
                    world: Mat4::from_translation(root),
                    parent: Mat4::IDENTITY,
                    tail: Vec3::new(0., -0.26, 0.03),
                    stiffness: 38.,
                    damping: 10.,
                    gravity: 1.8,
                    max_angle: 0.9,
                    radius: 0.008,
                };
                std::hint::black_box(s.advance(input, &colliders, 1. / 60.).unwrap());
            }
            if frame >= 300 {
                times.push(begin.elapsed().as_secs_f64() * 1000.);
            }
        }
        times.sort_by(f64::total_cmp);
        println!(
            "joints={count} capsules=8 frames=1000 mean_ms={:.6} p95_ms={:.6} max_ms={:.6}",
            times.iter().sum::<f64>() / times.len() as f64,
            times[949],
            times[999]
        );
    }
}
