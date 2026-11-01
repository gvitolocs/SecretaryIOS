use glam::{Mat4, Vec3};
use secretary_avatar_com3d2::{
    cloth::{Grid, Parameters},
    secondary_physics::Capsule,
    Result,
};
fn main() -> Result<()> {
    let mut grid = Grid::new(
        24,
        5,
        Parameters {
            iterations: 18,
            ..Parameters::default()
        },
    )?;
    for row in 0..5 {
        for col in 0..24 {
            let t = row as f32 / 4.;
            let a = col as f32 * std::f32::consts::TAU / 24.;
            let r = 0.14 + 0.20 * t;
            let i = row * 24 + col;
            grid.set_pose(
                i,
                Mat4::from_translation(Vec3::new(r * a.sin(), 1. - 0.5 * t, r * a.cos())),
                Mat4::IDENTITY,
                if row == 0 { -1 } else { (i - 24) as i32 },
                1.,
            )?;
        }
    }
    let colliders = [Capsule {
        a: Vec3::new(0., 0.55, 0.),
        b: Vec3::new(0., 1.1, 0.),
        radius: 0.105,
    }; 8];
    for _ in 0..60 {
        grid.advance(&colliders, 1. / 60., true)?;
    }
    let mut samples = Vec::new();
    for _ in 0..3600 {
        let t = std::time::Instant::now();
        grid.advance(&colliders, 1. / 60., true)?;
        samples.push(t.elapsed().as_secs_f64() * 1000.);
    }
    samples.sort_by(f64::total_cmp);
    println!(
        "{}",
        serde_json::json!({"nodes":120,"iterations":18,"capsules":8,"frames":3600,"medianMs":samples[1800],"p95Ms":samples[3420],"p99Ms":samples[3564],"maxDropMetres":grid.max_drop,"contacts":grid.contacts,"scope":"native CPU solver only; excludes browser/GPU; synthetic 24x5 cloth"})
    );
    Ok(())
}
