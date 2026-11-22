// SPDX-License-Identifier: AGPL-3.0-or-later
//! The display list a laid-out tree paints into.

use serde::Serialize;

use crate::geometry::{Color, Point, Rect, Size};
use crate::node::{Direction, Icon, Node, Paint, TextAlign};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FontWeight {
    #[default]
    Regular,
    SemiBold,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FontFamily {
    #[default]
    Sans,
    Mono,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct TextStyle {
    pub size: f32,
    pub weight: FontWeight,
    pub family: FontFamily,
}

/// One drawing operation, in logical pixels. `clip` limits drawing to a
/// rectangle.
#[derive(Debug, Clone, PartialEq)]
pub enum Primitive {
    Rect {
        rect: Rect,
        color: Color,
        /// Corner radii: top-left, top-right, bottom-right, bottom-left.
        radius: [f32; 4],
        border: Option<(f32, Color)>,
        clip: Option<Rect>,
    },
    /// A linear two-color fill, `start` to `end` along `direction` (`Row`:
    /// left to right, `Column`: top to bottom). No border.
    Gradient {
        rect: Rect,
        start: Color,
        end: Color,
        direction: Direction,
        radius: [f32; 4],
        clip: Option<Rect>,
    },
    /// A soft drop shadow around `rect`.
    Shadow {
        rect: Rect,
        radius: f32,
        blur: f32,
        color: Color,
    },
    Line {
        from: Point,
        to: Point,
        width: f32,
        color: Color,
        clip: Option<Rect>,
    },
    Triangle {
        points: [Point; 3],
        color: Color,
        clip: Option<Rect>,
    },
    /// One line of text, vertically centered in `rect` and aligned
    /// horizontally by `align`.
    Text {
        rect: Rect,
        text: String,
        style: TextStyle,
        color: Color,
        align: TextAlign,
        clip: Option<Rect>,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scene {
    /// Logical size of the drawing area.
    pub size: Size,
    pub primitives: Vec<Primitive>,
}

impl Scene {
    pub(crate) fn paint(root: &Node, size: Size) -> Self {
        let mut scene = Self {
            size,
            primitives: Vec::new(),
        };
        scene.node(root, None);
        scene
    }

    fn node(&mut self, node: &Node, clip: Option<Rect>) {
        let rect = node.rect;
        let style = &node.style;
        if let Some((blur, color)) = style.shadow {
            self.primitives.push(Primitive::Shadow {
                rect,
                radius: style.radius.into_iter().fold(0.0, f32::max),
                blur,
                color,
            });
        }
        if let (Some(bg), Some((end, direction))) = (style.background, style.gradient) {
            self.primitives.push(Primitive::Gradient {
                rect,
                start: bg,
                end,
                direction,
                radius: style.radius,
                clip,
            });
        } else if let Some(bg) = style.background {
            self.primitives.push(Primitive::Rect {
                rect,
                color: bg,
                radius: style.radius,
                border: style.border,
                clip,
            });
        } else if let Some(border) = style.border {
            self.primitives.push(Primitive::Rect {
                rect,
                color: Color::TRANSPARENT,
                radius: style.radius,
                border: Some(border),
                clip,
            });
        }
        let content = rect.inset(node.padding);
        match &node.paint {
            Some(Paint::Icon(icon)) => self.icon(*icon, content, style.color, clip),
            Some(Paint::Meter {
                levels,
                vertical,
                background,
                low,
                mid,
                high,
            }) => self.meter(
                *levels,
                *vertical,
                *background,
                (*low, *mid, *high),
                content,
                clip,
            ),
            Some(Paint::Waveform { columns, color }) => {
                self.waveform(columns, *color, content, clip)
            }
            Some(Paint::Polyline {
                points,
                width,
                color,
            }) => {
                let at = |p: &Point| Point::new(content.x + p.x, content.y + p.y);
                for pair in points.windows(2) {
                    self.line(at(&pair[0]), at(&pair[1]), *width, *color, clip);
                }
            }
            Some(Paint::Rects { rects, color }) => {
                for r in rects.iter() {
                    self.primitives.push(Primitive::Rect {
                        rect: Rect::new(content.x + r.x, content.y + r.y, r.w, r.h),
                        color: *color,
                        radius: [1.0; 4],
                        border: None,
                        clip,
                    });
                }
            }
            Some(Paint::Knob {
                value,
                default,
                bipolar,
                accent,
                body,
                rim,
                tick,
            }) => self.knob(
                *value, *default, *bipolar, *accent, *body, *rim, *tick, content, clip,
            ),
            None => {}
        }
        if let Some(text) = &node.text {
            // A clipping node clips its own text too (long names).
            let text_clip = if node.clip {
                Some(clip.map_or(rect, |c| c.intersect(&rect)))
            } else {
                clip
            };
            self.primitives.push(Primitive::Text {
                rect: content,
                text: text.clone(),
                style: style.text_style(),
                color: style.color,
                align: style.text_align,
                clip: text_clip,
            });
        }
        let child_clip = if node.clip {
            Some(clip.map_or(rect, |c| c.intersect(&rect)))
        } else {
            clip
        };
        for child in &node.children {
            self.node(child, child_clip);
        }
    }

    fn line(&mut self, from: Point, to: Point, width: f32, color: Color, clip: Option<Rect>) {
        self.primitives.push(Primitive::Line {
            from,
            to,
            width,
            color,
            clip,
        });
    }

    fn icon(&mut self, icon: Icon, area: Rect, color: Color, clip: Option<Rect>) {
        // Icons are drawn in a centered square.
        let s = area.w.min(area.h).clamp(6.0, 12.0);
        let c = area.center();
        let (l, t, r, b) = (c.x - s / 2.0, c.y - s / 2.0, c.x + s / 2.0, c.y + s / 2.0);
        let w = 1.25;
        match icon {
            Icon::Close => {
                self.line(Point::new(l, t), Point::new(r, b), w, color, clip);
                self.line(Point::new(r, t), Point::new(l, b), w, color, clip);
            }
            Icon::Minimize => {
                self.line(Point::new(l, c.y), Point::new(r, c.y), w, color, clip);
            }
            Icon::Maximize => self.outline(Rect::new(l, t, s, s), w, color, clip),
            Icon::Restore => {
                let o = s * 0.25;
                self.outline(Rect::new(l, t + o, s - o, s - o), w, color, clip);
                self.line(
                    Point::new(l + o, t + o),
                    Point::new(l + o, t),
                    w,
                    color,
                    clip,
                );
                self.line(Point::new(l + o, t), Point::new(r, t), w, color, clip);
                self.line(Point::new(r, t), Point::new(r, b - o), w, color, clip);
                self.line(
                    Point::new(r, b - o),
                    Point::new(r - o, b - o),
                    w,
                    color,
                    clip,
                );
            }
            Icon::Play => self.primitives.push(Primitive::Triangle {
                points: [
                    Point::new(l + 1.0, t),
                    Point::new(r, c.y),
                    Point::new(l + 1.0, b),
                ],
                color,
                clip,
            }),
            Icon::Record => self.primitives.push(Primitive::Rect {
                rect: Rect::new(l, t, s, s),
                color,
                radius: [s / 2.0; 4],
                border: None,
                clip,
            }),
            Icon::Stop => self.primitives.push(Primitive::Rect {
                rect: Rect::new(l + 1.0, t + 1.0, s - 2.0, s - 2.0),
                color,
                radius: [1.0; 4],
                border: None,
                clip,
            }),
        }
    }

    fn outline(&mut self, rect: Rect, width: f32, color: Color, clip: Option<Rect>) {
        self.primitives.push(Primitive::Rect {
            rect,
            color: Color::TRANSPARENT,
            radius: [0.0; 4],
            border: Some((width, color)),
            clip,
        });
    }

    /// Two horizontal bars (left, right), filled to their level.
    fn meter(
        &mut self,
        levels: [f32; 2],
        vertical: bool,
        background: Color,
        (low, mid, high): (Color, Color, Color),
        area: Rect,
        clip: Option<Rect>,
    ) {
        let thickness = if vertical { area.w } else { area.h };
        let bar = ((thickness - 2.0) / 2.0).max(1.0);
        for (i, level) in levels.into_iter().enumerate() {
            let offset = i as f32 * (bar + 2.0);
            let track = if vertical {
                Rect::new(area.x + offset, area.y, bar, area.h)
            } else {
                Rect::new(area.x, area.y + offset, area.w, bar)
            };
            self.primitives.push(Primitive::Rect {
                rect: track,
                color: background,
                radius: [1.5; 4],
                border: None,
                clip,
            });
            let level = level.clamp(0.0, 1.0);
            if level > 0.0 {
                let color = if level > 0.95 {
                    high
                } else if level > 0.8 {
                    mid
                } else {
                    low
                };
                let fill = if vertical {
                    let h = track.h * level;
                    Rect::new(track.x, track.bottom() - h, track.w, h)
                } else {
                    Rect::new(track.x, track.y, track.w * level, track.h)
                };
                self.primitives.push(Primitive::Rect {
                    rect: fill,
                    color,
                    radius: [1.5; 4],
                    border: None,
                    clip,
                });
            }
        }
    }

    /// One vertical bar per column, spanning the column's min..max.
    fn waveform(&mut self, columns: &[(f32, f32)], color: Color, area: Rect, clip: Option<Rect>) {
        let mid = area.y + area.h / 2.0;
        let half = area.h / 2.0;
        for (i, &(min, max)) in columns.iter().enumerate() {
            let x = area.x + i as f32;
            if x >= area.right() {
                break;
            }
            let top = mid - max.clamp(-1.0, 1.0) * half;
            let bottom = mid - min.clamp(-1.0, 1.0) * half;
            // At least a hairline, so silence still shows the center line.
            let h = (bottom - top).max(1.0);
            self.primitives.push(Primitive::Rect {
                rect: Rect::new(x, top.min(mid - 0.5), 1.0, h),
                color,
                radius: [0.0; 4],
                border: None,
                clip,
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn knob(
        &mut self,
        value: f32,
        default: f32,
        bipolar: bool,
        accent: Color,
        body: Color,
        rim: Color,
        tick: Color,
        area: Rect,
        clip: Option<Rect>,
    ) {
        let value = value.clamp(0.0, 1.0);
        let default = default.clamp(0.0, 1.0);
        let c = area.center();
        let r = (area.w.min(area.h) * 0.5 - 1.0).max(8.0);
        let knob_r = (r - 4.5).max(5.0);
        let start_angle = 135.0_f32.to_radians();
        let sweep = 270.0_f32.to_radians();
        let angle_at = |u: f32| start_angle + u * sweep;

        // 11 scale ticks around the 270-degree travel.
        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let lit = if bipolar {
                if value >= 0.5 {
                    t >= 0.49 && t <= value + 0.02
                } else {
                    t <= 0.51 && t >= value - 0.02
                }
            } else {
                t <= value + 0.02 && (value > 0.01 || i == 0)
            };
            let a = angle_at(t);
            let (sin, cos) = a.sin_cos();
            let p0 = Point::new(c.x + cos * (r - 2.6), c.y + sin * (r - 2.6));
            let p1 = Point::new(c.x + cos * r, c.y + sin * r);
            self.line(
                p0,
                p1,
                if lit { 1.6 } else { 1.1 },
                if lit { accent } else { tick },
                clip,
            );
        }

        // Subtle marker dot at the parameter's default position.
        let def_a = angle_at(default);
        let def_dot = Point::new(c.x + def_a.cos() * (r - 1.2), c.y + def_a.sin() * (r - 1.2));
        self.primitives.push(Primitive::Rect {
            rect: Rect::new(def_dot.x - 1.0, def_dot.y - 1.0, 2.0, 2.0),
            color: rim.mix(accent, 0.45),
            radius: [1.0; 4],
            border: None,
            clip,
        });

        let knob_rect = Rect::new(c.x - knob_r, c.y - knob_r, knob_r * 2.0, knob_r * 2.0);
        self.primitives.push(Primitive::Shadow {
            rect: Rect::new(knob_rect.x, knob_rect.y + 1.5, knob_rect.w, knob_rect.h),
            radius: knob_r,
            blur: 4.0,
            color: Color::rgba(0.0, 0.0, 0.0, 0.45),
        });
        self.primitives.push(Primitive::Rect {
            rect: knob_rect,
            color: body,
            radius: [knob_r; 4],
            border: Some((1.5, rim)),
            clip,
        });

        let cap_r = knob_r * 0.72;
        self.primitives.push(Primitive::Rect {
            rect: Rect::new(c.x - cap_r, c.y - cap_r, cap_r * 2.0, cap_r * 2.0),
            color: body.mix(rim, 0.2),
            radius: [cap_r; 4],
            border: None,
            clip,
        });

        let a = angle_at(value);
        let (sin, cos) = a.sin_cos();
        let inner = knob_r * 0.20;
        let outer = knob_r - 1.5;
        self.line(
            Point::new(c.x + cos * inner, c.y + sin * inner),
            Point::new(c.x + cos * outer, c.y + sin * outer),
            2.0,
            accent,
            clip,
        );
    }
}
