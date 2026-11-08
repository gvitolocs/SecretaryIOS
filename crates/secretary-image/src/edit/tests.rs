use super::*;

fn solid(w: u32, h: u32, rgb: [u8; 3]) -> Frame {
    let rgba = (0..w * h)
        .flat_map(|_| [rgb[0], rgb[1], rgb[2], 255])
        .collect();
    Frame {
        width: w,
        height: h,
        rgba,
        complete: true,
    }
}

/// A picture where every pixel is different enough to notice a misplaced one.
fn pattern(w: u32, h: u32) -> Frame {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[
                (x * 255 / w) as u8,
                (y * 255 / h) as u8,
                ((x * 7 + y * 13) % 256) as u8,
                255,
            ]);
        }
    }
    Frame {
        width: w,
        height: h,
        rgba,
        complete: true,
    }
}

fn px(f: &Frame, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * f.width + x) * 4) as usize;
    [f.rgba[i], f.rgba[i + 1], f.rgba[i + 2], f.rgba[i + 3]]
}

fn edit(f: impl FnOnce(&mut EditList)) -> EditList {
    let mut e = EditList::default();
    f(&mut e);
    e
}

fn encode(v: f32) -> u8 {
    (linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0 + 0.5) as u8
}

fn mean_luma(f: &Frame) -> f32 {
    let sum: f32 = f
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p[0] as f32 * 0.2126 + p[1] as f32 * 0.7152 + p[2] as f32 * 0.0722)
        .sum();
    sum / (f.rgba.len() / 4) as f32
}

fn fnv(f: &Frame) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in f
        .width
        .to_le_bytes()
        .into_iter()
        .chain(f.height.to_le_bytes())
        .chain(f.rgba.iter().copied())
    {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    h
}

// ---- the list ----------------------------------------------------------------------------------

#[test]
fn the_default_edit_is_the_identity_and_returns_the_same_pixels() {
    let src = pattern(61, 47);
    let e = EditList::default();
    assert!(e.is_identity());
    let out = render(&src, &e).unwrap();
    assert_eq!((out.width, out.height), (61, 47));
    assert_eq!(out.rgba, src.rgba);
    // A crop of the whole frame, a zero-strength filter and a zero slider are still identities.
    let e = edit(|e| {
        e.crop = Some(Crop::FULL);
        e.filter = Some(FilterRef {
            name: "vivid".into(),
            strength: 0.0,
        });
    });
    assert!(e.is_identity());
    assert_eq!(render(&src, &e).unwrap().rgba, src.rgba);
}

#[test]
fn the_colour_step_itself_is_lossless_when_nothing_changes() {
    // The 8-bit → linear → 8-bit round trip must not shift any level, or a tiny slider move would
    // posterise the whole picture.
    let mut f = Frame {
        width: 256,
        height: 1,
        rgba: (0..=255u8).flat_map(|v| [v, 255 - v, v, 255]).collect(),
        complete: true,
    };
    let before = f.rgba.clone();
    colour(
        &mut f,
        &Adjust {
            exposure: 1e-6,
            ..Adjust::default()
        },
    );
    assert_eq!(f.rgba, before);
}

#[test]
fn the_per_channel_tables_give_the_same_pixels_as_the_general_path() {
    let src = pattern(64, 48);
    for a in [
        Adjust {
            exposure: 0.7,
            ..Adjust::default()
        },
        Adjust {
            exposure: -1.3,
            warmth: 40.0,
            ..Adjust::default()
        },
        Adjust {
            contrast: 35.0,
            warmth: -20.0,
            tint: 9.0,
            fade: 15.0,
            ..Adjust::default()
        },
        Adjust {
            contrast: -60.0,
            tint: -40.0,
            ..Adjust::default()
        },
    ] {
        let (mut fast, mut general) = (src.clone(), src.clone());
        colour_with(&mut fast, &a, false);
        colour_with(&mut general, &a, true);
        assert_eq!(fast.rgba, general.rgba, "{a:?}");
        assert_ne!(fast.rgba, src.rgba);
    }
}

#[test]
fn the_tone_table_covers_the_whole_range() {
    // Black, white, NaN and out-of-range values index inside the table.
    for v in [
        0.0f32,
        1e-30,
        6e-5,
        0.5,
        1.0,
        7.0,
        -3.0,
        f32::NAN,
        f32::INFINITY,
    ] {
        assert!(tone_index(v) < TONE_LEN, "{v}");
    }
    assert_eq!(tone_index(f32::NAN), 0);
    assert_eq!(tone_index(1.0), TONE_LEN - 1);
    let t = tone_table(&Adjust::default());
    assert_eq!((t[0], t[TONE_LEN - 1]), (0, 255));
    assert!(t.windows(2).all(|w| w[0] <= w[1]), "monotonic");
}

#[test]
fn json_round_trips_and_is_checked() {
    let e = EditList {
        rotate: 1,
        straighten: -3.5,
        crop: Some(Crop {
            x: 0.1,
            y: 0.2,
            w: 0.5,
            h: 0.6,
        }),
        adjust: Adjust {
            exposure: 0.5,
            contrast: 10.0,
            warmth: -20.0,
            ..Adjust::default()
        },
        filter: Some(FilterRef {
            name: "noir".into(),
            strength: 0.5,
        }),
        markup: vec![
            Mark::Pen {
                color: "#ff3b30".into(),
                width: 0.006,
                points: vec![[0.1, 0.1], [0.2, 0.2]],
            },
            Mark::Text {
                color: "#fff".into(),
                size: 0.05,
                x: 0.1,
                y: 0.8,
                text: "Ciao".into(),
            },
        ],
        ..EditList::default()
    };
    let json = serde_json::to_string(&e).unwrap();
    assert_eq!(EditList::from_json(&json).unwrap(), e);
    // Missing fields are the defaults.
    assert!(EditList::from_json(r#"{"v":1}"#).unwrap().is_identity());
    assert!(EditList::from_json("{}").unwrap().is_identity());

    for (bad_json, why) in [
        (r#"{"v":2}"#, "version"),
        (r#"{"v":1,"rotate":4}"#, "rotate"),
        (r#"{"v":1,"straighten":46}"#, "straighten"),
        (r#"{"v":1,"straighten":null}"#, "null number"),
        (
            r#"{"v":1,"crop":{"x":0.6,"y":0,"w":0.6,"h":1}}"#,
            "crop outside",
        ),
        (r#"{"v":1,"crop":{"x":0,"y":0,"w":0,"h":1}}"#, "empty crop"),
        (r#"{"v":1,"adjust":{"exposure":4}}"#, "exposure"),
        (r#"{"v":1,"adjust":{"fade":-1}}"#, "fade"),
        (r#"{"v":1,"adjust":{"sparkle":1}}"#, "unknown adjustment"),
        (r#"{"v":1,"filter":{"name":"nope"}}"#, "filter"),
        (
            r#"{"v":1,"filter":{"name":"vivid","strength":2}}"#,
            "strength",
        ),
        (
            r#"{"v":1,"markup":[{"t":"pen","color":"red","width":0.01,"points":[[0,0]]}]}"#,
            "colour",
        ),
        (
            r##"{"v":1,"markup":[{"t":"pen","color":"#fff","width":0.01,"points":[]}]}"##,
            "empty stroke",
        ),
        (
            r##"{"v":1,"markup":[{"t":"pen","color":"#fff","width":0.5,"points":[[0,0]]}]}"##,
            "wide",
        ),
        (
            r##"{"v":1,"markup":[{"t":"star","color":"#fff"}]}"##,
            "mark type",
        ),
        ("not json", "syntax"),
    ] {
        assert!(
            EditList::from_json(bad_json).is_err(),
            "{why} should be refused: {bad_json}"
        );
    }
    let many = EditList {
        markup: (0..=MAX_MARKS)
            .map(|_| Mark::Pen {
                color: "#fff".into(),
                width: 0.01,
                points: vec![[0.0, 0.0]],
            })
            .collect(),
        ..EditList::default()
    };
    assert!(many.validate().is_err());
    let long = EditList {
        markup: vec![Mark::Text {
            color: "#fff".into(),
            size: 0.05,
            x: 0.0,
            y: 0.0,
            text: "x".repeat(MAX_TEXT + 1),
        }],
        ..EditList::default()
    };
    assert!(long.validate().is_err());
    let nan = EditList {
        straighten: f32::NAN,
        ..EditList::default()
    };
    assert!(nan.validate().is_err());
}

#[test]
fn render_refuses_bad_pictures() {
    let e = EditList::default();
    assert!(render(
        &Frame {
            width: 0,
            height: 4,
            rgba: vec![],
            complete: true
        },
        &e
    )
    .is_err());
    assert!(render(
        &Frame {
            width: 2,
            height: 2,
            rgba: vec![0; 7],
            complete: true
        },
        &e
    )
    .is_err());
    assert!(render(&solid(2, 2, [1, 2, 3]), &EditList { v: 9, ..e }).is_err());
}

// ---- geometry ----------------------------------------------------------------------------------

#[test]
fn quarter_turns_turn_clockwise_and_four_make_a_circle() {
    let src = pattern(6, 4);
    let once = render(&src, &edit(|e| e.rotate = 1)).unwrap();
    assert_eq!((once.width, once.height), (4, 6));
    // Clockwise: the top left corner goes to the top right, the bottom left to the top left.
    assert_eq!(px(&once, 3, 0), px(&src, 0, 0));
    assert_eq!(px(&once, 0, 0), px(&src, 0, 3));
    assert_eq!(px(&once, 3, 5), px(&src, 5, 0));
    let twice = render(&src, &edit(|e| e.rotate = 2)).unwrap();
    assert_eq!((twice.width, twice.height), (6, 4));
    assert_eq!(px(&twice, 0, 0), px(&src, 5, 3));
    let three = render(&src, &edit(|e| e.rotate = 3)).unwrap();
    assert_eq!(px(&three, 0, 5), px(&src, 0, 0));
    assert_eq!(px(&three, 3, 0), px(&src, 5, 3));
    let mut f = src.clone();
    for _ in 0..4 {
        f = render(&f, &edit(|e| e.rotate = 1)).unwrap();
    }
    assert_eq!(f.rgba, src.rgba);
}

#[test]
fn crop_cuts_a_box_of_the_frame() {
    let src = pattern(100, 80);
    let e = edit(|e| {
        e.crop = Some(Crop {
            x: 0.25,
            y: 0.5,
            w: 0.5,
            h: 0.25,
        })
    });
    let out = render(&src, &e).unwrap();
    assert_eq!((out.width, out.height), (50, 20));
    for (x, y) in [(0, 0), (49, 19), (10, 7)] {
        assert_eq!(px(&out, x, y), px(&src, 25 + x, 40 + y));
    }
    assert_eq!(
        layout(100, 80, &e),
        Layout {
            turned_w: 100,
            turned_h: 80,
            frame_w: 100,
            frame_h: 80,
            crop_x: 25,
            crop_y: 40,
            out_w: 50,
            out_h: 20
        }
    );
    // Rotating first: the crop box is in the turned frame.
    let e = edit(|e| {
        e.rotate = 1;
        e.crop = Some(Crop {
            x: 0.0,
            y: 0.0,
            w: 0.5,
            h: 1.0,
        });
    });
    let out = render(&src, &e).unwrap();
    assert_eq!((out.width, out.height), (40, 100));
    assert_eq!(
        px(&out, 39, 0),
        px(&src, 0, 40),
        "turned (39, 0) is the source's (0, 40)"
    );
    // A crop that rounds to nothing still leaves one pixel.
    let tiny = edit(|e| {
        e.crop = Some(Crop {
            x: 0.99,
            y: 0.99,
            w: 0.005,
            h: 0.005,
        })
    });
    let out = render(&src, &tiny).unwrap();
    assert!(out.width >= 1 && out.height >= 1);
}

#[test]
fn straightening_keeps_the_aspect_and_never_shows_the_corners() {
    // At 45 degrees a square keeps 1/(cos + sin) = 0.7071 of its side.
    let l = layout(1000, 1000, &edit(|e| e.straighten = 45.0));
    assert_eq!((l.frame_w, l.frame_h), (707, 707));
    // A 3:2 picture keeps its aspect.
    for deg in [-30.0f32, -7.5, 0.4, 12.0, 25.0, 44.0] {
        let (w, h) = (3000u32, 2000u32);
        let l = layout(w, h, &edit(|e| e.straighten = deg));
        assert!(
            (l.frame_w as f64 / l.frame_h as f64 - 1.5).abs() < 0.01,
            "{deg}: {l:?}"
        );
        // All four corners of the frame, traced back, lie inside the picture; a frame 1% larger
        // would not.
        let theta = (deg as f64).to_radians();
        let reach = |fw: f64, fh: f64| {
            let (s, c) = (theta.sin().abs(), theta.cos());
            (fw / 2.0 * c + fh / 2.0 * s, fw / 2.0 * s + fh / 2.0 * c)
        };
        let (rx, ry) = reach(l.frame_w as f64, l.frame_h as f64);
        assert!(
            rx <= w as f64 / 2.0 + 1.0 && ry <= h as f64 / 2.0 + 1.0,
            "{deg}: {rx} {ry}"
        );
        if deg.abs() > 1.0 {
            let (rx, ry) = reach(l.frame_w as f64 * 1.01, l.frame_h as f64 * 1.01);
            assert!(
                rx > w as f64 / 2.0 || ry > h as f64 / 2.0,
                "{deg}: the frame could be larger"
            );
        }
    }
    // Below a thousandth of a degree nothing is resampled.
    let src = pattern(40, 30);
    assert_eq!(
        render(&src, &edit(|e| e.straighten = 0.0005)).unwrap().rgba,
        src.rgba
    );
}

#[test]
fn positive_straighten_turns_the_picture_clockwise() {
    // A bright horizontal line through the middle of a dark picture.
    let (w, h) = (300u32, 200u32);
    let mut src = solid(w, h, [0, 0, 0]);
    for y in 99..101 {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            src.rgba[i..i + 3].copy_from_slice(&[255, 255, 255]);
        }
    }
    let out = render(&src, &edit(|e| e.straighten = 15.0)).unwrap();
    // The line's centre of mass in a column.
    let centre = |x: u32| {
        let (mut sum, mut weight) = (0.0f32, 0.0f32);
        for y in 0..out.height {
            let v = px(&out, x, y)[0] as f32;
            sum += v * y as f32;
            weight += v;
        }
        sum / weight
    };
    let (left, right) = (centre(out.width / 8), centre(out.width * 7 / 8));
    assert!(
        right > left + 20.0,
        "clockwise: the right end goes down ({left} {right})"
    );
    // Counter-clockwise is the mirror image.
    let out = render(&src, &edit(|e| e.straighten = -15.0)).unwrap();
    let centre = |x: u32| {
        let (mut sum, mut weight) = (0.0f32, 0.0f32);
        for y in 0..out.height {
            let v = px(&out, x, y)[0] as f32;
            sum += v * y as f32;
            weight += v;
        }
        sum / weight
    };
    assert!(centre(out.width / 8) > centre(out.width * 7 / 8) + 20.0);
}

#[test]
fn straightening_a_flat_picture_leaves_it_flat() {
    let src = solid(120, 80, [200, 100, 50]);
    let out = render(&src, &edit(|e| e.straighten = 20.0)).unwrap();
    assert!(
        out.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [200, 100, 50, 255]),
        "no empty corners, no edge bleed"
    );
}

#[test]
fn layout_predicts_the_rendered_size() {
    let src = pattern(97, 61);
    for rotate in 0..4u8 {
        for straighten in [0.0f32, 9.0, -31.0] {
            for crop in [
                None,
                Some(Crop {
                    x: 0.13,
                    y: 0.07,
                    w: 0.61,
                    h: 0.83,
                }),
            ] {
                let e = edit(|e| {
                    e.rotate = rotate;
                    e.straighten = straighten;
                    e.crop = crop;
                });
                let out = render(&src, &e).unwrap();
                let l = layout(97, 61, &e);
                assert_eq!((out.width, out.height), (l.out_w, l.out_h), "{e:?}");
                assert!(l.crop_x + l.out_w <= l.frame_w && l.crop_y + l.out_h <= l.frame_h);
            }
        }
    }
}

// ---- colour ------------------------------------------------------------------------------------

#[test]
fn plus_one_ev_doubles_linear_light() {
    for v in [10u8, 50, 90, 120] {
        let src = solid(4, 4, [v, v, v]);
        let lin = srgb_to_linear(v as f32 / 255.0);
        for (ev, factor) in [(1.0, 2.0), (-1.0, 0.5), (2.0, 4.0)] {
            let out = render(&src, &edit(|e| e.adjust.exposure = ev)).unwrap();
            let want = encode(lin * factor);
            let got = px(&out, 0, 0);
            assert!(
                (got[0] as i32 - want as i32).abs() <= 1,
                "{v} at {ev} EV: {got:?} vs {want}"
            );
            assert_eq!(got[3], 255);
        }
    }
}

#[test]
fn values_clamp_instead_of_wrapping_and_alpha_is_left_alone() {
    let mut src = solid(2, 1, [240, 10, 130]);
    src.rgba[3] = 77;
    let out = render(&src, &edit(|e| e.adjust.exposure = 3.0)).unwrap();
    assert_eq!(
        &px(&out, 0, 0)[..3],
        &[255, 43, 255][..],
        "255 stays 255, the 10 becomes 43"
    );
    assert_eq!(px(&out, 0, 0)[3], 77);
    let dark = render(&src, &edit(|e| e.adjust.exposure = -3.0)).unwrap();
    assert!(px(&dark, 0, 0)[0] < 100 && px(&dark, 0, 0)[1] <= 5);
    // Out-of-range sliders from a filter stack are clamped, not wrapped.
    let e = edit(|e| {
        e.adjust.contrast = 100.0;
        e.adjust.saturation = 100.0;
        e.filter = Some(FilterRef {
            name: "noir".into(),
            strength: 1.0,
        });
    });
    let a = e.effective_adjust();
    assert!(a.contrast <= 100.0 && a.saturation >= -100.0);
    render(&src, &e).unwrap();
}

#[test]
fn contrast_spreads_around_mid_grey() {
    let mid = 118u8; // sRGB 0.46, linear 0.18
    let src = Frame {
        width: 3,
        height: 1,
        rgba: vec![60, 60, 60, 255, mid, mid, mid, 255, 190, 190, 190, 255],
        complete: true,
    };
    let more = render(&src, &edit(|e| e.adjust.contrast = 50.0)).unwrap();
    assert!(px(&more, 0, 0)[0] < 60 && px(&more, 2, 0)[0] > 190);
    assert!(
        (px(&more, 1, 0)[0] as i32 - mid as i32).abs() <= 1,
        "mid grey stays"
    );
    let less = render(&src, &edit(|e| e.adjust.contrast = -50.0)).unwrap();
    assert!(px(&less, 0, 0)[0] > 60 && px(&less, 2, 0)[0] < 190);
}

#[test]
fn shadows_and_highlights_work_on_their_own_ranges() {
    let src = Frame {
        width: 3,
        height: 1,
        rgba: vec![30, 30, 30, 255, 118, 118, 118, 255, 235, 235, 235, 255],
        complete: true,
    };
    let lifted = render(&src, &edit(|e| e.adjust.shadows = 80.0)).unwrap();
    assert!(
        px(&lifted, 0, 0)[0] > 30 + 20,
        "dark pixel lifted: {:?}",
        px(&lifted, 0, 0)
    );
    assert!(px(&lifted, 2, 0)[0] <= 237, "highlights barely move");
    let recovered = render(&src, &edit(|e| e.adjust.highlights = -80.0)).unwrap();
    assert!(
        px(&recovered, 2, 0)[0] < 235 - 15,
        "bright pixel pulled down: {:?}",
        px(&recovered, 2, 0)
    );
    assert!(
        (px(&recovered, 0, 0)[0] as i32 - 30).abs() <= 1,
        "shadows untouched"
    );
    let deepened = render(&src, &edit(|e| e.adjust.shadows = -80.0)).unwrap();
    assert!(px(&deepened, 0, 0)[0] < 30);
}

#[test]
fn saturation_warmth_and_tint() {
    let src = solid(2, 2, [180, 120, 90]);
    let grey = render(&src, &edit(|e| e.adjust.saturation = -100.0)).unwrap();
    let g = px(&grey, 0, 0);
    assert!(g[0] == g[1] && g[1] == g[2], "{g:?}");
    let vivid = render(&src, &edit(|e| e.adjust.saturation = 60.0)).unwrap();
    let v = px(&vivid, 0, 0);
    assert!(v[0] as i32 - v[2] as i32 > 180 - 90, "{v:?}");
    let warm = px(
        &render(&src, &edit(|e| e.adjust.warmth = 60.0)).unwrap(),
        0,
        0,
    );
    assert!(warm[0] > 180 && warm[2] < 90, "{warm:?}");
    let cool = px(
        &render(&src, &edit(|e| e.adjust.warmth = -60.0)).unwrap(),
        0,
        0,
    );
    assert!(cool[0] < 180 && cool[2] > 90, "{cool:?}");
    let magenta = px(
        &render(&src, &edit(|e| e.adjust.tint = 60.0)).unwrap(),
        0,
        0,
    );
    assert!(magenta[1] < 120, "{magenta:?}");
    let green = px(
        &render(&src, &edit(|e| e.adjust.tint = -60.0)).unwrap(),
        0,
        0,
    );
    assert!(green[1] > 120, "{green:?}");
    // White balance keeps the brightness: a grey stays about as bright.
    let grey_in = solid(2, 2, [128, 128, 128]);
    let warmed = px(
        &render(&grey_in, &edit(|e| e.adjust.warmth = 100.0)).unwrap(),
        0,
        0,
    );
    let luma = warmed[0] as f32 * 0.2126 + warmed[1] as f32 * 0.7152 + warmed[2] as f32 * 0.0722;
    assert!((luma - 128.0).abs() < 8.0, "{warmed:?} {luma}");
}

#[test]
fn vignette_darkens_the_corners_and_not_the_centre() {
    let src = solid(101, 101, [180, 180, 180]);
    let out = render(&src, &edit(|e| e.adjust.vignette = 80.0)).unwrap();
    assert_eq!(px(&out, 50, 50)[0], 180);
    assert!(px(&out, 0, 0)[0] < 120, "{:?}", px(&out, 0, 0));
    assert!(
        px(&out, 0, 50)[0] < 180 && px(&out, 0, 50)[0] > px(&out, 0, 0)[0],
        "the edge middle is between"
    );
    let light = render(&src, &edit(|e| e.adjust.vignette = -80.0)).unwrap();
    assert!(px(&light, 0, 0)[0] > 180);
}

#[test]
fn fade_lifts_the_blacks() {
    let src = solid(2, 2, [0, 0, 0]);
    let out = render(&src, &edit(|e| e.adjust.fade = 100.0)).unwrap();
    assert!(px(&out, 0, 0)[0] > 40, "{:?}", px(&out, 0, 0));
    assert_eq!(
        render(&src, &edit(|e| e.adjust.fade = 0.0)).unwrap().rgba,
        src.rgba
    );
}

#[test]
fn filters_are_parameter_sets_on_top_of_the_sliders() {
    let src = pattern(32, 24);
    for (name, _) in FILTERS {
        let e = edit(|e| {
            e.filter = Some(FilterRef {
                name: (*name).into(),
                strength: 1.0,
            })
        });
        let out = render(&src, &e).unwrap();
        assert_ne!(out.rgba, src.rgba, "{name} changes the picture");
        // The same as typing its parameters into the sliders.
        let by_hand = edit(|e| e.adjust = filter_params(name).unwrap());
        assert_eq!(out.rgba, render(&src, &by_hand).unwrap().rgba, "{name}");
    }
    // Strength scales; half strength is between none and full.
    let half = render(
        &src,
        &edit(|e| {
            e.filter = Some(FilterRef {
                name: "bw".into(),
                strength: 0.5,
            })
        }),
    )
    .unwrap();
    let px0 = px(&half, 20, 5);
    assert!(
        px0[0] != px0[2],
        "still some colour at half strength: {px0:?}"
    );
    let bw = render(
        &src,
        &edit(|e| {
            e.filter = Some(FilterRef {
                name: "bw".into(),
                strength: 1.0,
            })
        }),
    )
    .unwrap();
    let p = px(&bw, 20, 5);
    assert!(p[0] == p[1] && p[1] == p[2]);
    // The sliders add to the filter.
    let both = edit(|e| {
        e.filter = Some(FilterRef {
            name: "warm".into(),
            strength: 1.0,
        });
        e.adjust.exposure = 0.5;
    });
    assert!((both.effective_adjust().exposure - 0.55).abs() < 1e-6);
}

// ---- auto enhance ------------------------------------------------------------------------------

fn gradient_between(lo: u8, hi: u8) -> Frame {
    let (w, h) = (64u32, 32u32);
    let mut rgba = Vec::new();
    for _ in 0..h {
        for x in 0..w {
            let v = lo as u32 + (hi - lo) as u32 * x / (w - 1);
            rgba.extend_from_slice(&[v as u8, v as u8, v as u8, 255]);
        }
    }
    Frame {
        width: w,
        height: h,
        rgba,
        complete: true,
    }
}

#[test]
fn auto_enhance_brightens_a_dark_picture() {
    let dark = gradient_between(2, 60);
    let a = auto_enhance(&Histogram::of(&dark));
    assert!(a.exposure > 0.5, "{a:?}");
    assert!(a.shadows > 0.0, "{a:?}");
    let out = render(
        &dark,
        &EditList {
            adjust: a,
            ..EditList::default()
        },
    )
    .unwrap();
    assert!(
        mean_luma(&out) > mean_luma(&dark) + 25.0,
        "{} -> {}",
        mean_luma(&dark),
        mean_luma(&out)
    );
    a.clamped();
    assert_eq!(a, a.clamped(), "already within range");
}

#[test]
fn auto_enhance_calms_a_blown_out_picture_and_leaves_a_good_one() {
    let bright = gradient_between(170, 255);
    let a = auto_enhance(&Histogram::of(&bright));
    assert!(a.exposure < -0.3 && a.highlights < 0.0, "{a:?}");
    let out = render(
        &bright,
        &EditList {
            adjust: a,
            ..EditList::default()
        },
    )
    .unwrap();
    assert!(mean_luma(&out) < mean_luma(&bright) - 10.0);

    let good = gradient_between(5, 250);
    let a = auto_enhance(&Histogram::of(&good));
    assert!(
        a.exposure.abs() < 0.3 && a.contrast == 0.0 && a.shadows < 10.0 && a.highlights > -30.0,
        "{a:?}"
    );

    // A flat, narrow picture asks for contrast.
    let flat = gradient_between(100, 150);
    assert!(auto_enhance(&Histogram::of(&flat)).contrast > 10.0);
    assert_eq!(auto_enhance(&Histogram::new()), Adjust::default());
}

#[test]
fn auto_enhance_corrects_a_colour_cast_only_partly() {
    // A strong orange cast.
    let f = solid(32, 32, [200, 120, 60]);
    let a = auto_enhance(&Histogram::of(&f));
    assert!(a.warmth < -5.0 && a.warmth >= -25.0, "{a:?}");
    // Grey pictures get no saturation boost and no cast correction.
    let grey = auto_enhance(&Histogram::of(&gradient_between(20, 230)));
    assert_eq!((grey.saturation, grey.warmth, grey.tint), (0.0, 0.0, 0.0));
}

// ---- markup ------------------------------------------------------------------------------------

fn red_count(f: &Frame) -> usize {
    f.rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] > 200 && p[1] < 60 && p[2] < 60)
        .count()
}

#[test]
fn a_pen_stroke_is_drawn_anti_aliased_on_its_line_only() {
    let src = solid(200, 100, [255, 255, 255]);
    let e = edit(|e| {
        e.markup = vec![Mark::Pen {
            color: "#ff0000".into(),
            width: 0.04,
            points: vec![[0.1, 0.5075], [0.9, 0.5075]],
        }]
    });
    let out = render(&src, &e).unwrap();
    // 0.04 of 200 px = 8 px wide, its edges between pixel rows (50.75 ± 4).
    assert_eq!(&px(&out, 100, 50)[..3], &[255, 0, 0][..]);
    assert_eq!(&px(&out, 100, 40)[..3], &[255, 255, 255][..]);
    assert_eq!(
        &px(&out, 5, 50)[..3],
        &[255, 255, 255][..],
        "starts at x = 20"
    );
    let col: Vec<u8> = (44..56).map(|y| px(&out, 100, y)[1]).collect();
    assert!(
        col.iter().any(|g| *g > 10 && *g < 245),
        "soft edges: {col:?}"
    );
    let painted = (0..100).filter(|y| px(&out, 100, *y)[1] < 128).count();
    assert!((8..=9).contains(&painted), "{painted}");
    // The stroke has round caps.
    assert!(px(&out, 17, 50)[1] < 128 && px(&out, 14, 50)[1] > 128);
}

#[test]
fn a_stroke_crossing_itself_is_not_darker_where_it_crosses() {
    let src = solid(100, 100, [255, 255, 255]);
    let pts = vec![[0.1, 0.5], [0.9, 0.5], [0.5, 0.1], [0.5, 0.9], [0.1, 0.5]];
    let e = edit(|e| {
        e.markup = vec![Mark::Highlighter {
            color: "#ff0000".into(),
            width: 0.1,
            points: pts,
        }]
    });
    let out = render(&src, &e).unwrap();
    let on_line = px(&out, 70, 50);
    let crossing = px(&out, 50, 50);
    assert_eq!(
        on_line, crossing,
        "one coat of highlighter, however often it is crossed"
    );
    assert!(
        on_line[1] > 100 && on_line[1] < 255,
        "see-through: {on_line:?}"
    );
}

#[test]
fn the_highlighter_lets_what_is_under_it_show() {
    let mut src = solid(100, 40, [255, 255, 255]);
    for x in 0..100 {
        let i = ((20 * 100 + x) * 4) as usize;
        src.rgba[i..i + 3].copy_from_slice(&[0, 0, 0]);
    }
    let e = edit(|e| {
        e.markup = vec![Mark::Highlighter {
            color: "#ffeb3b".into(),
            width: 0.2,
            points: vec![[0.1, 0.5], [0.9, 0.5]],
        }]
    });
    let out = render(&src, &e).unwrap();
    let text_line = px(&out, 50, 20);
    let paper = px(&out, 50, 25);
    assert!(
        text_line[0] < 150,
        "the dark line is still dark: {text_line:?}"
    );
    assert!(
        paper[2] < 255 && paper[0] > 200,
        "yellow tint on paper: {paper:?}"
    );
}

#[test]
fn an_arrow_has_a_head_wider_than_its_shaft() {
    let src = solid(300, 200, [255, 255, 255]);
    let e = edit(|e| {
        e.markup = vec![Mark::Arrow {
            color: "#ff0000".into(),
            width: 0.02,
            from: [0.1, 0.5],
            to: [0.9, 0.5],
        }]
    });
    let out = render(&src, &e).unwrap();
    let thickness = |x: u32| (0..200).filter(|y| px(&out, x, *y)[1] < 128).count();
    let shaft = thickness(100);
    let head = thickness(250);
    assert!((4..=8).contains(&shaft), "shaft {shaft}");
    assert!(head > shaft * 2, "head {head} vs shaft {shaft}");
    // The tip is at the end point; nothing past it.
    assert!(px(&out, 267, 100)[1] < 128);
    assert_eq!(&px(&out, 280, 100)[..3], &[255, 255, 255][..]);
    // A zero-length arrow is a dot, not a crash.
    let dot = edit(|e| {
        e.markup = vec![Mark::Arrow {
            color: "#ff0000".into(),
            width: 0.05,
            from: [0.5, 0.5],
            to: [0.5, 0.5],
        }]
    });
    assert!(red_count(&render(&src, &dot).unwrap()) > 20);
}

#[test]
fn text_is_drawn_with_the_embedded_font_and_accents_work() {
    let src = solid(400, 200, [30, 30, 30]);
    let mark = |text: &str| Mark::Text {
        color: "#ffffff".into(),
        size: 0.1,
        x: 0.05,
        y: 0.1,
        text: text.into(),
    };
    let out = render(&src, &edit(|e| e.markup = vec![mark("Hello")])).unwrap();
    let white = |f: &Frame| {
        f.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] > 200)
            .count()
    };
    assert!(white(&out) > 150, "{}", white(&out));
    // All ink sits in the box of the line: starts at x = 20, y = 20, 40 px tall.
    for y in 0..200u32 {
        for x in 0..400u32 {
            if px(&out, x, y)[0] > 200 {
                assert!(
                    (18..140).contains(&x) && (18..75).contains(&y),
                    "ink at {x},{y}"
                );
            }
        }
    }
    let other = render(&src, &edit(|e| e.markup = vec![mark("Ciao àèé")])).unwrap();
    assert_ne!(other.rgba, out.rgba);
    assert!(white(&other) > white(&out));
    // Two lines are taller than one.
    let two = render(&src, &edit(|e| e.markup = vec![mark("Hi\nthere")])).unwrap();
    assert!((0..200u32).any(|y| y > 75 && (0..400u32).any(|x| px(&two, x, y)[0] > 200)));
    // Characters the font lacks do not crash or leave boxes.
    render(&src, &edit(|e| e.markup = vec![mark("日本 \u{1F600}")])).unwrap();
}

#[test]
fn markup_stays_on_the_picture_when_the_crop_moves() {
    let src = solid(200, 100, [255, 255, 255]);
    let dot = Mark::Pen {
        color: "#ff0000".into(),
        width: 0.05,
        points: vec![[0.75, 0.5]],
    };
    let whole = render(&src, &edit(|e| e.markup = vec![dot.clone()])).unwrap();
    assert_eq!(px(&whole, 150, 50)[1], 0);
    // Crop to the right half: the dot at x = 150 is now at x = 50.
    let right = render(
        &src,
        &edit(|e| {
            e.crop = Some(Crop {
                x: 0.5,
                y: 0.0,
                w: 0.5,
                h: 1.0,
            });
            e.markup = vec![dot.clone()];
        }),
    )
    .unwrap();
    assert_eq!((right.width, right.height), (100, 100));
    assert_eq!(px(&right, 50, 50)[1], 0);
    // Crop to the left half: the dot is outside, nothing is drawn.
    let left = render(
        &src,
        &edit(|e| {
            e.crop = Some(Crop {
                x: 0.0,
                y: 0.0,
                w: 0.5,
                h: 1.0,
            });
            e.markup = vec![dot.clone()];
        }),
    )
    .unwrap();
    assert_eq!(red_count(&left), 0);
    // Marks partly outside the frame are cut, not wrapped.
    let edge = edit(|e| {
        e.markup = vec![Mark::Pen {
            color: "#ff0000".into(),
            width: 0.1,
            points: vec![[1.0, 0.5], [1.4, 0.5]],
        }]
    });
    let out = render(&src, &edge).unwrap();
    assert!(red_count(&out) > 0 && px(&out, 196, 50)[1] == 0);
}

#[test]
fn markup_is_drawn_on_the_finished_colours() {
    let src = solid(60, 60, [128, 128, 128]);
    let e = edit(|e| {
        e.adjust.saturation = -100.0;
        e.adjust.exposure = 1.0;
        e.markup = vec![Mark::Pen {
            color: "#ff0000".into(),
            width: 0.2,
            points: vec![[0.5, 0.5]],
        }];
    });
    let out = render(&src, &e).unwrap();
    assert_eq!(
        &px(&out, 30, 30)[..3],
        &[255, 0, 0][..],
        "the ink keeps its exact colour"
    );
}

// ---- determinism -------------------------------------------------------------------------------

fn busy_edit() -> EditList {
    EditList {
        rotate: 3,
        straighten: 7.25,
        crop: Some(Crop {
            x: 0.1,
            y: 0.05,
            w: 0.8,
            h: 0.9,
        }),
        adjust: Adjust {
            exposure: 0.4,
            contrast: 20.0,
            highlights: -30.0,
            shadows: 25.0,
            saturation: 15.0,
            warmth: 12.0,
            tint: -6.0,
            vignette: 30.0,
            fade: 10.0,
        },
        filter: Some(FilterRef {
            name: "vintage".into(),
            strength: 0.6,
        }),
        markup: vec![
            Mark::Pen {
                color: "#34c759".into(),
                width: 0.01,
                points: vec![[0.2, 0.2], [0.4, 0.5], [0.7, 0.3]],
            },
            Mark::Highlighter {
                color: "#ffeb3b".into(),
                width: 0.04,
                points: vec![[0.2, 0.7], [0.8, 0.7]],
            },
            Mark::Arrow {
                color: "#ffffff".into(),
                width: 0.008,
                from: [0.1, 0.9],
                to: [0.4, 0.6],
            },
            Mark::Text {
                color: "#ffffff".into(),
                size: 0.06,
                x: 0.3,
                y: 0.1,
                text: "Lucina".into(),
            },
        ],
        ..EditList::default()
    }
}

#[test]
fn rendering_is_deterministic() {
    let src = pattern(173, 131);
    let e = busy_edit();
    let first = render(&src, &e).unwrap();
    let second = render(&src, &e).unwrap();
    assert_eq!(fnv(&first), fnv(&second));
    // Through JSON and back: the same bytes.
    let again = EditList::from_json(&serde_json::to_string(&e).unwrap()).unwrap();
    assert_eq!(fnv(&render(&src, &again).unwrap()), fnv(&first));
    // The input is not touched, and a different input gives a different hash.
    assert_eq!(src.rgba, pattern(173, 131).rgba);
    assert_ne!(fnv(&render(&pattern(173, 132), &e).unwrap()), fnv(&first));
    // Every output byte is within 0..=255 by construction; alpha is opaque like the input.
    assert!(first.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
}

#[test]
fn a_big_stack_of_edits_renders_a_big_picture() {
    // Not a benchmark (see examples/bench_edit.rs): only that the whole pipeline holds up on a
    // phone-sized picture, odd dimensions included.
    let src = pattern(1601, 1203);
    let out = render(&src, &busy_edit()).unwrap();
    let l = layout(1601, 1203, &busy_edit());
    assert_eq!((out.width, out.height), (l.out_w, l.out_h));
}
