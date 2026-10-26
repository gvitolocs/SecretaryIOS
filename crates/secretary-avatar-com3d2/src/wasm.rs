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
            let child=if io[150]>0.5{match crate::body_physics::child_matrix(input.bone_correction,m(128),Vec3::from_slice(&io[144..147]),Vec3::from_slice(&io[147..150]),out.sub_roll){Ok(m)=>Some(m.to_cols_array()),Err(_)=>return -1}}else{None};
            unsafe {
                for i in 0..16 {
                    IO[80 + i] = matrix[i];
                }
                IO[96] = out.angle_degrees;
                IO[97] = out.sub_roll;
                IO[98] = out.steps as f32;
                if let Some(child)=child{for i in 0..16{IO[160+i]=child[i];}}
            }
            0
        }
        Err(_) => -1,
    }
}
