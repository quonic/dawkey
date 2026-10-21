// SPDX-License-Identifier: AGPL-3.0-or-later
//! Renders on the software adapter, so results match across machines.

use dk_text::TextSystem;
use dk_ui::{
    Color, FontFamily, FontWeight, Point, Primitive, Rect, Scene, Size, TextAlign, TextStyle,
};

use crate::{Gpu, GpuPreference, Image, Renderer};

/// One device for every test: opening lavapipe devices on several threads at
/// once can crash the driver.
fn gpu() -> &'static Gpu {
    static GPU: std::sync::OnceLock<Gpu> = std::sync::OnceLock::new();
    GPU.get_or_init(|| Gpu::headless(GpuPreference::Software).expect("software Vulkan (lavapipe)"))
}

fn render(scene: &Scene, scale: f32) -> Image {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu.device);
    let mut text = TextSystem::new();
    renderer
        .render_image(gpu, &mut text, scene, scale, Color::TRANSPARENT)
        .unwrap()
}

fn rect(rect: Rect, color: Color) -> Primitive {
    Primitive::Rect {
        rect,
        color,
        radius: [0.0; 4],
        border: None,
        clip: None,
    }
}

#[test]
fn draws_rects_borders_and_clips() {
    let red = Color::hex(0xff0000);
    let scene = Scene {
        size: Size::new(40.0, 20.0),
        primitives: vec![
            rect(Rect::new(0.0, 0.0, 10.0, 10.0), red),
            Primitive::Rect {
                rect: Rect::new(20.0, 0.0, 10.0, 10.0),
                color: Color::hex(0x0000ff),
                radius: [0.0; 4],
                border: Some((2.0, Color::hex(0x00ff00))),
                clip: None,
            },
            Primitive::Rect {
                rect: Rect::new(0.0, 10.0, 40.0, 10.0),
                color: red,
                radius: [0.0; 4],
                border: None,
                clip: Some(Rect::new(0.0, 10.0, 5.0, 10.0)),
            },
        ],
    };
    let img = render(&scene, 2.0);
    assert_eq!((img.width, img.height), (80, 40));
    assert_eq!(img.pixel(10, 10), [255, 0, 0, 255]);
    // Exactly to the edge, no bleed.
    assert_eq!(img.pixel(19, 19), [255, 0, 0, 255]);
    assert_eq!(img.pixel(20, 10), [0, 0, 0, 0]);
    // Border (4 physical px) around a blue fill.
    assert_eq!(img.pixel(41, 10), [0, 255, 0, 255]);
    assert_eq!(img.pixel(50, 10), [0, 0, 255, 255]);
    // Clipped to the first 10 physical px.
    assert_eq!(img.pixel(5, 30), [255, 0, 0, 255]);
    assert_eq!(img.pixel(15, 30), [0, 0, 0, 0]);
}

#[test]
fn rounded_corners_and_shapes_are_antialiased() {
    let white = Color::hex(0xffffff);
    let scene = Scene {
        size: Size::new(40.0, 20.0),
        primitives: vec![
            Primitive::Rect {
                rect: Rect::new(0.0, 0.0, 20.0, 20.0),
                color: white,
                radius: [8.0, 0.0, 0.0, 0.0],
                border: None,
                clip: None,
            },
            Primitive::Line {
                from: Point::new(22.0, 10.0),
                to: Point::new(38.0, 10.0),
                width: 2.0,
                color: white,
                clip: None,
            },
        ],
    };
    let img = render(&scene, 1.0);
    // Top-left corner is cut away; top-right is square.
    assert_eq!(img.pixel(0, 0)[3], 0);
    assert_eq!(img.pixel(19, 0)[3], 255);
    // The arc crosses the diagonal near (2.3, 2.3): partial coverage there.
    let curve: Vec<u8> = (0..6).map(|i| img.pixel(i, i)[3]).collect();
    assert!(curve.iter().any(|&a| a > 0 && a < 255), "{curve:?}");
    // The 2 px line covers rows 9 and 10.
    assert_eq!(img.pixel(30, 9), [255, 255, 255, 255]);
    assert_eq!(img.pixel(30, 12)[3], 0);
}

#[test]
fn draws_text() {
    let scene = Scene {
        size: Size::new(100.0, 30.0),
        primitives: vec![Primitive::Text {
            rect: Rect::new(0.0, 0.0, 100.0, 30.0),
            text: "Dawkey".into(),
            style: TextStyle {
                size: 16.0,
                weight: FontWeight::SemiBold,
                family: FontFamily::Sans,
            },
            color: Color::hex(0xffffff),
            align: TextAlign::Center,
            clip: None,
        }],
    };
    let img = render(&scene, 1.0);
    let lit: Vec<(u32, u32)> = (0..img.height)
        .flat_map(|y| (0..img.width).map(move |x| (x, y)))
        .filter(|&(x, y)| img.pixel(x, y)[3] > 128)
        .collect();
    assert!(lit.len() > 100, "{} lit pixels", lit.len());
    // Centered: ink starts and ends a similar distance from the edges.
    let min_x = lit.iter().map(|p| p.0).min().unwrap();
    let max_x = lit.iter().map(|p| p.0).max().unwrap();
    assert!(
        (min_x as i32 - (99 - max_x) as i32).abs() <= 3,
        "{min_x}..{max_x}"
    );
    let min_y = lit.iter().map(|p| p.1).min().unwrap();
    let max_y = lit.iter().map(|p| p.1).max().unwrap();
    assert!(min_y >= 5 && max_y <= 24, "{min_y}..{max_y}");
    assert!(img.to_png().starts_with(b"\x89PNG"));
}
