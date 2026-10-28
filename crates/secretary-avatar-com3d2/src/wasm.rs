//! Minimal synchronous WASM bridge. No generated JavaScript or Node runtime.
//! IO: base-world[0..16], parent-world[16..32], bone correction[32..48],
//! parent correction[48..64], size/softness/support/blend/inward/elevation[64..70].
//! Output: bone-local matrix[80..96], angle[96], sub-roll[97], steps[98].
use crate::body_physics::{BodySpring, Input, Parameters};
use glam::{Mat4, Vec3};
use std::sync::Mutex;
static STATES: Mutex<[BodySpring; 2]> = Mutex::new([BodySpring::new(), BodySpring::new()]);
static mut IO: [f32; 192] = [0.; 192];
#[no_mangle]
pub extern "C" fn skin_color(rgb: u32) -> i32 {
    if rgb > 0xffffff {
        return -1;
    }
    let color = std::array::from_fn::<_, 3, _>(|i| {
        crate::skin_color::linear(((rgb >> (16 - i * 8)) & 255) as f32 / 255.)
            * crate::skin_color::DETAIL_GAIN
    });
    unsafe {
        for (i, c) in color.into_iter().enumerate() {
            IO[110 + i] = c;
        }
        IO[113] = 1.;
    }
    0
}
#[no_mangle]
pub extern "C" fn body_io_ptr() -> *mut f32 {
    std::ptr::addr_of_mut!(IO).cast::<f32>()
}
#[no_mangle]
pub extern "C" fn body_reset() {
    if let Ok(mut states) = STATES.lock() {
        for s in states.iter_mut() {
            s.reset();
        }
    }
}
#[no_mangle]
pub extern "C" fn body_impulse(index: u32, x: f32, y: f32, z: f32) -> i32 {
    let Ok(mut states) = STATES.lock() else {
        return -1;
    };
    let Some(state) = states.get_mut(index as usize) else {
        return -1;
    };
    if state.impulse(Vec3::new(x, y, z)).is_ok() {
        0
    } else {
        -1
    }
}
#[no_mangle]
pub extern "C" fn body_step(index: u32, dt: f32) -> i32 {
    // The host writes this fixed IO block only between synchronous calls. It is
    // copied before evaluation; Rust retains no references into host-writable IO.
    let io = unsafe { IO };
    let Ok(mut states) = STATES.lock() else {
        return -1;
    };
    let Some(state) = states.get_mut(index as usize) else {
        return -1;
    };
    let m = |start| Mat4::from_cols_array(io[start..start + 16].try_into().unwrap());
    let input = Input {
        base_world: m(0),
        parent_world: m(16),
        bone_correction: m(32),
        parent_correction: m(48),
    };
    let p = Parameters {
        size: io[64],
        softness: io[65],
        supported: io[66] > 0.5,
        blend: io[67],
        inward: io[68],
        elevation: io[69],
        side: if index == 1 { -1. } else { 1. },
        gravity: 0.1,
    };
    match state.advance(input, p, dt) {
        Ok(out) => {
            let matrix = out.local_matrix.to_cols_array();
            let child = if io[150] > 0.5 {
                match crate::body_physics::child_matrix(
                    input.bone_correction,
                    m(128),
                    Vec3::from_slice(&io[144..147]),
                    Vec3::from_slice(&io[147..150]),
                    out.sub_roll,
                ) {
                    Ok(m) => Some(m.to_cols_array()),
                    Err(_) => return -1,
                }
            } else {
                None
            };
            unsafe {
                for i in 0..16 {
                    IO[80 + i] = matrix[i];
                }
                IO[96] = out.angle_degrees;
                IO[97] = out.sub_roll;
                IO[98] = out.steps as f32;
                if let Some(child) = child {
                    for i in 0..16 {
                        IO[160 + i] = child[i];
                    }
                }
            }
            0
        }
        Err(_) => -1,
    }
}
static SECONDARY: Mutex<[crate::secondary_physics::Spring; 256]> =
    Mutex::new([crate::secondary_physics::Spring::new(); 256]);
static mut SECONDARY_IO: [f32; 192] = [0.; 192];
#[no_mangle]
pub extern "C" fn secondary_io_ptr() -> *mut f32 {
    std::ptr::addr_of_mut!(SECONDARY_IO).cast::<f32>()
}
#[no_mangle]
pub extern "C" fn secondary_reset() {
    if let Ok(mut states) = SECONDARY.lock() {
        for s in states.iter_mut() {
            s.reset();
        }
    }
}
#[no_mangle]
pub extern "C" fn secondary_impulse(index: u32, x: f32, y: f32, z: f32) -> i32 {
    let Ok(mut states) = SECONDARY.lock() else {
        return -1;
    };
    let Some(s) = states.get_mut(index as usize) else {
        return -1;
    };
    if s.impulse(Vec3::new(x, y, z)).is_ok() {
        0
    } else {
        -1
    }
}
#[no_mangle]
pub extern "C" fn secondary_step(index: u32, dt: f32) -> i32 {
    let io = unsafe { SECONDARY_IO };
    let Ok(mut states) = SECONDARY.lock() else {
        return -1;
    };
    let Some(state) = states.get_mut(index as usize) else {
        return -1;
    };
    let count = io[40] as usize;
    if count > 16 {
        return -1;
    }
    let mut capsules = [crate::secondary_physics::Capsule {
        a: Vec3::ZERO,
        b: Vec3::ZERO,
        radius: 0.,
    }; 16];
    for (i, c) in capsules.iter_mut().enumerate().take(count) {
        let p = 48 + i * 7;
        *c = crate::secondary_physics::Capsule {
            a: Vec3::from_slice(&io[p..p + 3]),
            b: Vec3::from_slice(&io[p + 3..p + 6]),
            radius: io[p + 6],
        };
    }
    let input = crate::secondary_physics::Input {
        world: Mat4::from_cols_array(io[0..16].try_into().unwrap()),
        parent: Mat4::from_cols_array(io[16..32].try_into().unwrap()),
        tail: Vec3::from_slice(&io[32..35]),
        stiffness: io[35],
        damping: io[36],
        gravity: io[37],
        max_angle: io[38],
        radius: io[39],
    };
    match state.advance(input, &capsules[..count], dt) {
        Ok(out) => {
            unsafe {
                SECONDARY_IO[160..176].copy_from_slice(&out.local.to_cols_array());
                SECONDARY_IO[176] = out.angle;
                SECONDARY_IO[177] = out.contacts as f32;
                SECONDARY_IO[178] = out.drop_meters;
            }
            0
        }
        Err(_) => -1,
    }
}
// Batch cloth ABI: 8 profile values, capsule count/gravity toggle at 8/9;
// stats at 10..14; 16 capsules at 16..128; full world/parent matrices per node.
const CLOTH_WORLD: usize = 128;
const CLOTH_PARENT: usize = CLOTH_WORLD + 32 * crate::cloth::MAX_NODES;
const CLOTH_GRAVITY: usize = CLOTH_PARENT + crate::cloth::MAX_NODES;
const CLOTH_OUTPUT: usize = CLOTH_GRAVITY + crate::cloth::MAX_NODES;
const CLOTH_LEN: usize = CLOTH_OUTPUT + 16 * crate::cloth::MAX_NODES;
static mut CLOTH_IO: [f32; CLOTH_LEN] = [0.; CLOTH_LEN];
static CLOTH: Mutex<[Option<crate::cloth::Grid>; 4]> = Mutex::new([const { None }; 4]);
#[no_mangle]
pub extern "C" fn cloth_io_ptr() -> *mut f32 {
    std::ptr::addr_of_mut!(CLOTH_IO).cast::<f32>()
}
#[no_mangle]
pub extern "C" fn cloth_io_len() -> u32 {
    CLOTH_LEN as u32
}
#[no_mangle]
pub extern "C" fn cloth_world_offset() -> u32 {
    CLOTH_WORLD as u32
}
#[no_mangle]
pub extern "C" fn cloth_parent_offset() -> u32 {
    CLOTH_PARENT as u32
}
#[no_mangle]
pub extern "C" fn cloth_gravity_offset() -> u32 {
    CLOTH_GRAVITY as u32
}
#[no_mangle]
pub extern "C" fn cloth_output_offset() -> u32 {
    CLOTH_OUTPUT as u32
}
#[no_mangle]
pub extern "C" fn cloth_clear() {
    if let Ok(mut grids) = CLOTH.lock() {
        for g in grids.iter_mut() {
            *g = None;
        }
    }
}
#[no_mangle]
pub extern "C" fn cloth_reset() {
    if let Ok(mut grids) = CLOTH.lock() {
        for g in grids.iter_mut().flatten() {
            g.reset();
        }
    }
}
fn cloth_pose(
    grid: &mut crate::cloth::Grid,
) -> crate::Result<([crate::secondary_physics::Capsule; 16], usize, bool)> {
    // Synchronous browser calls cannot overlap this immutable input view and the
    // later output write. The view ends before the solver/output stage.
    let io = unsafe { &*std::ptr::addr_of!(CLOTH_IO) };
    for i in 0..grid.len() {
        let base = CLOTH_WORLD + 32 * i;
        let parent = io[CLOTH_PARENT + i];
        if !parent.is_finite() || parent.fract() != 0. {
            return Err(crate::invalid("cloth parent index"));
        }
        grid.set_pose(
            i,
            Mat4::from_cols_array(io[base..base + 16].try_into().unwrap()),
            Mat4::from_cols_array(io[base + 16..base + 32].try_into().unwrap()),
            parent as i32,
            io[CLOTH_GRAVITY + i],
        )?;
    }
    let count = io[8];
    if !count.is_finite()
        || count.fract() != 0.
        || !(0.0..=16.).contains(&count)
        || ![0., 1.].contains(&io[9])
    {
        return Err(crate::invalid("cloth frame flags"));
    }
    let mut caps = [crate::secondary_physics::Capsule {
        a: Vec3::ZERO,
        b: Vec3::ZERO,
        radius: 0.,
    }; 16];
    for (i, c) in caps.iter_mut().enumerate().take(count as usize) {
        let p = 16 + i * 7;
        *c = crate::secondary_physics::Capsule {
            a: Vec3::from_slice(&io[p..p + 3]),
            b: Vec3::from_slice(&io[p + 3..p + 6]),
            radius: io[p + 6],
        };
    }
    Ok((caps, count as usize, io[9] == 1.))
}
#[no_mangle]
pub extern "C" fn cloth_init(slot: u32, columns: u32, rows: u32) -> i32 {
    let Ok(mut states) = CLOTH.lock() else {
        return -1;
    };
    let Some(state) = states.get_mut(slot as usize) else {
        return -1;
    };
    let p = unsafe { std::slice::from_raw_parts(std::ptr::addr_of!(CLOTH_IO).cast::<f32>(), 8) };
    if !p[0].is_finite() || p[0].fract() != 0. {
        return -1;
    }
    let closed = unsafe { CLOTH_IO[14] };
    if ![0., 1.].contains(&closed) {
        return -1;
    }
    let params = crate::cloth::Parameters {
        closed: closed == 1.,
        iterations: p[0] as usize,
        damping: p[1],
        horizontal_compliance: p[2],
        vertical_compliance: p[3],
        bend_compliance: p[4],
        support: p[5],
        radius: p[6],
        floor: p[7],
    };
    let Ok(mut grid) = crate::cloth::Grid::new(columns as usize, rows as usize, params) else {
        return -1;
    };
    let Ok((caps, count, gravity)) = cloth_pose(&mut grid) else {
        return -1;
    };
    if grid.advance(&caps[..count], 0., gravity).is_err() {
        return -1;
    }
    *state = Some(grid);
    0
}
#[no_mangle]
pub extern "C" fn cloth_step(slot: u32, dt: f32) -> i32 {
    let Ok(mut states) = CLOTH.lock() else {
        return -1;
    };
    let Some(Some(grid)) = states.get_mut(slot as usize) else {
        return -1;
    };
    let Ok((caps, count, gravity)) = cloth_pose(grid) else {
        return -1;
    };
    let Ok(matrices) = grid.advance(&caps[..count], dt, gravity) else {
        return -1;
    };
    unsafe {
        let out = std::ptr::addr_of_mut!(CLOTH_IO).cast::<f32>();
        for (i, m) in matrices.iter().enumerate() {
            std::ptr::copy_nonoverlapping(
                m.to_cols_array().as_ptr(),
                out.add(CLOTH_OUTPUT + 16 * i),
                16,
            );
        }
    }
    unsafe {
        CLOTH_IO[10] = grid.contacts as f32;
        CLOTH_IO[11] = grid.max_drop;
        CLOTH_IO[12] = grid.steps as f32;
        CLOTH_IO[13] = grid.len() as f32;
    }
    0
}
#[no_mangle]
pub extern "C" fn cloth_impulse(slot: u32, x: f32, y: f32, z: f32) -> i32 {
    let Ok(mut states) = CLOTH.lock() else {
        return -1;
    };
    let Some(Some(grid)) = states.get_mut(slot as usize) else {
        return -1;
    };
    if grid.impulse(Vec3::new(x, y, z)).is_ok() {
        0
    } else {
        -1
    }
}

// Customization IO: controls[0..21], local[32..48], parent correction[48..64],
// bone correction[64..80], output[80..96]. ABI version 1, all floats.
static CUSTOM_IO: std::sync::Mutex<[f32; 128]> = std::sync::Mutex::new([0.; 128]);
#[no_mangle]
pub extern "C" fn customization_io_ptr() -> *mut f32 {
    CUSTOM_IO.lock().unwrap().as_mut_ptr()
}
#[no_mangle]
pub extern "C" fn customization_apply(index: u32) -> i32 {
    let names = crate::customization::bone_names();
    let Some(name) = names.get(index as usize) else {
        return -1;
    };
    let mut io = CUSTOM_IO.lock().unwrap();
    let values: [f32; 21] = io[0..21].try_into().unwrap();
    let matrix =
        |start: usize| glam::Mat4::from_cols_array(io[start..start + 16].try_into().unwrap());
    match crate::customization::evaluate(name, matrix(32), matrix(48), matrix(64), &values) {
        Ok(m) => {
            io[80..96].copy_from_slice(&m.to_cols_array());
            0
        }
        Err(_) => -1,
    }
}
