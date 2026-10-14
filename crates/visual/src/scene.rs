//! `ui.scene` 3D: the draw function gets a scene object whose methods add
//! world-space geometry (y up). Zeron frames the camera on the geometry,
//! orbits it on drag, shades faces with one soft light, and projects
//! everything to depth-sorted 2D polygons that the desktop paints like any
//! other vector shape, so a 3D view stays transparent over glass and uses the
//! theme's colors.

use mlua::{Function, Table, UserData, UserDataMethods, Value};

use crate::draw::lua_error;
use crate::node::{parse_color, value_text};
use crate::style::{Color, TextSize, Token};

pub const MAX_FACES: usize = 30_000;
const MAX_ITEMS: usize = 20_000;

pub type V3 = [f32; 3];

#[derive(Debug, Clone, PartialEq)]
struct Face {
    v: [V3; 4],
    /// 3 or 4 corners.
    n: u8,
    normal: V3,
    color: Color,
    /// Closed solids cull faces turned away from the camera; open surfaces
    /// and meshes light both sides.
    two_sided: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct Line3 {
    a: V3,
    b: V3,
    color: Color,
    width: f32,
}

#[derive(Debug, Clone, PartialEq)]
struct Point3 {
    at: V3,
    radius: f32,
    color: Color,
    hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct Label3 {
    at: V3,
    text: String,
    color: Color,
    size: TextSize,
}

/// The geometry one draw call produced.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Scene {
    faces: Vec<Face>,
    lines: Vec<Line3>,
    points: Vec<Point3>,
    labels: Vec<Label3>,
    pub time: f64,
}

/// The orbit camera: angles in radians around the geometry's center.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
}

/// One painted primitive, in canvas pixels.
#[derive(Debug, Clone, PartialEq)]
pub enum Prim {
    Face {
        points: Vec<[f32; 2]>,
        color: Color,
        /// Lighting, 0 (unlit) to 1 (facing the light).
        shade: f32,
    },
    Line {
        a: [f32; 2],
        b: [f32; 2],
        color: Color,
        width: f32,
    },
    Dot {
        at: [f32; 2],
        radius: f32,
        color: Color,
    },
    Label {
        at: [f32; 2],
        text: String,
        color: Color,
        size: TextSize,
    },
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Projection {
    /// Back to front; labels last.
    pub prims: Vec<Prim>,
    /// Hover hints: screen position, radius, text.
    pub hints: Vec<([f32; 2], f32, String)>,
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn normalize(a: V3) -> V3 {
    let len = dot(a, a).sqrt();
    if len < 1e-9 {
        [0.0, 1.0, 0.0]
    } else {
        scale(a, 1.0 / len)
    }
}

impl Scene {
    pub fn new(time: f64) -> Self {
        Self {
            time,
            ..Default::default()
        }
    }

    fn face(
        &mut self,
        corners: &[V3],
        color: Color,
        outward: Option<V3>,
        two_sided: bool,
    ) -> mlua::Result<()> {
        if self.faces.len() >= MAX_FACES {
            return Err(lua_error(format!(
                "a scene draws at most {MAX_FACES} faces"
            )));
        }
        if corners.iter().flatten().any(|c| !c.is_finite()) {
            return Err(lua_error("a scene vertex is NaN or infinite"));
        }
        let mut normal = normalize(cross(
            sub(corners[1], corners[0]),
            sub(corners[corners.len() - 1], corners[0]),
        ));
        if corners.len() == 4 {
            // A non-planar quad: average both diagonals' normals.
            normal = normalize(cross(
                sub(corners[2], corners[0]),
                sub(corners[3], corners[1]),
            ));
        }
        if let Some(outward) = outward
            && dot(normal, outward) < 0.0
        {
            normal = scale(normal, -1.0);
        }
        let mut v = [[0.0; 3]; 4];
        v[..corners.len()].copy_from_slice(corners);
        self.faces.push(Face {
            v,
            n: corners.len() as u8,
            normal,
            color,
            two_sided,
        });
        Ok(())
    }

    fn polygon(
        &mut self,
        corners: &[V3],
        color: Color,
        outward: Option<V3>,
        two_sided: bool,
    ) -> mlua::Result<()> {
        match corners.len() {
            0..=2 => Ok(()),
            3 | 4 => self.face(corners, color, outward, two_sided),
            _ => {
                for i in 1..corners.len() - 1 {
                    self.face(
                        &[corners[0], corners[i], corners[i + 1]],
                        color,
                        outward,
                        two_sided,
                    )?;
                }
                Ok(())
            }
        }
    }

    fn items(&self) -> usize {
        self.lines.len() + self.points.len() + self.labels.len()
    }

    fn check_items(&self) -> mlua::Result<()> {
        if self.items() >= MAX_ITEMS {
            return Err(lua_error(format!(
                "a scene draws at most {MAX_ITEMS} lines, points, and labels"
            )));
        }
        Ok(())
    }

    pub fn face_count(&self) -> usize {
        self.faces.len()
    }

    fn bounds(&self) -> Option<(V3, V3)> {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        let mut grow = |p: V3| {
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
        };
        for face in &self.faces {
            for v in &face.v[..face.n as usize] {
                grow(*v);
            }
        }
        for line in &self.lines {
            grow(line.a);
            grow(line.b);
        }
        for point in &self.points {
            grow(point.at);
        }
        for label in &self.labels {
            grow(label.at);
        }
        lo[0].is_finite().then_some((lo, hi))
    }

    /// Frame the geometry, orbit to `camera`, light, cull, and depth-sort.
    pub fn project(&self, camera: Camera, width: f32, height: f32) -> Projection {
        let Some((lo, hi)) = self.bounds() else {
            return Projection::default();
        };
        let target = scale(add(lo, hi), 0.5);
        let half = scale(sub(hi, lo), 0.5);
        let radius = dot(half, half).sqrt().max(1e-3);
        let pitch = camera.pitch.clamp(-1.52, 1.52);
        // Fit the geometry's extent as the turntable sees it: the footprint's
        // radius sideways, and its height plus the tilted footprint
        // vertically. Neither depends on yaw, so a spinning scene keeps its
        // size.
        let footprint = half[0].hypot(half[2]).max(1e-3);
        let tall = (half[1] * pitch.cos().abs() + footprint * pitch.sin().abs()).max(1e-3);
        let distance = radius * 4.0;
        let eye = add(
            target,
            scale(
                [
                    pitch.cos() * camera.yaw.sin(),
                    pitch.sin(),
                    pitch.cos() * camera.yaw.cos(),
                ],
                distance,
            ),
        );
        let forward = normalize(sub(target, eye));
        let right = normalize(cross(forward, [0.0, 1.0, 0.0]));
        let up = cross(right, forward);
        let focal = 0.86
            * (width / 2.0 / footprint).min(height / 2.0 / tall)
            * distance
            * camera.zoom.max(0.05);
        let near = distance * 0.02;
        let (cx, cy) = (width / 2.0, height / 2.0);
        let view = |p: V3| {
            let d = sub(p, eye);
            [dot(d, right), dot(d, up), dot(d, forward)]
        };
        let screen = |v: V3| [cx + focal * v[0] / v[2], cy - focal * v[1] / v[2]];
        // A soft key light from the upper left, in front of the geometry.
        let light = normalize(add(
            add(scale(right, -0.45), scale(up, 0.8)),
            scale(forward, -0.55),
        ));

        let mut sorted: Vec<(f32, Prim)> = Vec::with_capacity(self.faces.len() + self.items());
        for face in &self.faces {
            let corners = &face.v[..face.n as usize];
            let viewed: Vec<V3> = corners.iter().map(|v| view(*v)).collect();
            if viewed.iter().any(|v| v[2] < near) {
                continue;
            }
            let centroid = scale(
                corners.iter().fold([0.0; 3], |acc, v| add(acc, *v)),
                1.0 / corners.len() as f32,
            );
            let toward_eye = dot(face.normal, sub(eye, centroid));
            if !face.two_sided && toward_eye < 0.0 {
                continue;
            }
            let mut normal = face.normal;
            if toward_eye < 0.0 {
                normal = scale(normal, -1.0);
            }
            let lambert = dot(normal, light).max(0.0);
            let depth = viewed.iter().map(|v| v[2]).sum::<f32>() / viewed.len() as f32;
            sorted.push((
                depth,
                Prim::Face {
                    points: viewed.iter().map(|v| screen(*v)).collect(),
                    color: face.color,
                    shade: 0.35 + 0.65 * lambert,
                },
            ));
        }
        for line in &self.lines {
            let (a, b) = (view(line.a), view(line.b));
            if a[2] < near || b[2] < near {
                continue;
            }
            sorted.push((
                (a[2] + b[2]) / 2.0 - radius * 1e-3,
                Prim::Line {
                    a: screen(a),
                    b: screen(b),
                    color: line.color,
                    width: line.width,
                },
            ));
        }
        let mut hints = Vec::new();
        for point in &self.points {
            let v = view(point.at);
            if v[2] < near {
                continue;
            }
            let at = screen(v);
            if let Some(hint) = &point.hint {
                hints.push((at, point.radius.max(4.0), hint.clone()));
            }
            sorted.push((
                v[2] - radius * 2e-3,
                Prim::Dot {
                    at,
                    radius: point.radius,
                    color: point.color,
                },
            ));
        }
        sorted.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut prims: Vec<Prim> = sorted.into_iter().map(|(_, p)| p).collect();
        for label in &self.labels {
            let v = view(label.at);
            if v[2] < near {
                continue;
            }
            prims.push(Prim::Label {
                at: screen(v),
                text: label.text.clone(),
                color: label.color,
                size: label.size,
            });
        }
        Projection { prims, hints }
    }
}

fn v3(value: Value, what: &str) -> mlua::Result<V3> {
    match value {
        Value::Table(t) => {
            let get = |i: usize, key: &str| -> mlua::Result<f32> {
                match t.raw_get::<Option<f64>>(i)? {
                    Some(n) => Ok(n as f32),
                    None => Ok(t.raw_get::<Option<f64>>(key)?.unwrap_or(0.0) as f32),
                }
            };
            Ok([get(1, "x")?, get(2, "y")?, get(3, "z")?])
        }
        Value::Vector(v) => Ok([v.x(), v.y(), v.z()]),
        _ => Err(lua_error(format!("{what} must be a {{x, y, z}} list"))),
    }
}

fn color_of(options: &Table, default: Token) -> mlua::Result<Color> {
    Ok(parse_color(options.raw_get::<Value>("color")?)
        .map_err(|e| lua_error(e.to_string()))?
        .unwrap_or(Color::token(default)))
}

/// A ramp through `colors` (a list of tokens) at `t` in 0..1.
fn ramp(colors: &[Color], t: f32) -> Color {
    match colors.len() {
        0 => Color::token(Token::Accent),
        1 => colors[0],
        n => {
            let scaled = t.clamp(0.0, 1.0) * (n - 1) as f32;
            let i = (scaled.floor() as usize).min(n - 2);
            Color {
                token: colors[i].token,
                mix: Some((colors[i + 1].token, scaled - i as f32)),
                alpha: colors[i].alpha,
            }
        }
    }
}

fn colors_of(options: &Table) -> mlua::Result<Vec<Color>> {
    let Some(list) = options.raw_get::<Option<Table>>("colors")? else {
        return Ok(vec![color_of(options, Token::Accent)?]);
    };
    let mut out = Vec::new();
    for value in list.sequence_values::<Value>() {
        if let Some(color) = parse_color(value?).map_err(|e| lua_error(e.to_string()))? {
            out.push(color);
        }
    }
    Ok(out)
}

impl UserData for Scene {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("time", |_, s| Ok(s.time));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut("box", |_, s, o: Table| {
            let at = v3(o.raw_get("at")?, "box at")?;
            let size = match o.raw_get::<Value>("size")? {
                Value::Nil => [1.0; 3],
                Value::Integer(n) => [n as f32; 3],
                Value::Number(n) => [n as f32; 3],
                other => v3(other, "box size")?,
            };
            let color = color_of(&o, Token::Accent)?;
            let h = scale(size, 0.5);
            let c = |sx: f32, sy: f32, sz: f32| {
                [at[0] + sx * h[0], at[1] + sy * h[1], at[2] + sz * h[2]]
            };
            let quads = [
                (
                    [
                        c(-1., -1., 1.),
                        c(1., -1., 1.),
                        c(1., 1., 1.),
                        c(-1., 1., 1.),
                    ],
                    [0., 0., 1.],
                ),
                (
                    [
                        c(1., -1., -1.),
                        c(-1., -1., -1.),
                        c(-1., 1., -1.),
                        c(1., 1., -1.),
                    ],
                    [0., 0., -1.],
                ),
                (
                    [
                        c(1., -1., 1.),
                        c(1., -1., -1.),
                        c(1., 1., -1.),
                        c(1., 1., 1.),
                    ],
                    [1., 0., 0.],
                ),
                (
                    [
                        c(-1., -1., -1.),
                        c(-1., -1., 1.),
                        c(-1., 1., 1.),
                        c(-1., 1., -1.),
                    ],
                    [-1., 0., 0.],
                ),
                (
                    [
                        c(-1., 1., 1.),
                        c(1., 1., 1.),
                        c(1., 1., -1.),
                        c(-1., 1., -1.),
                    ],
                    [0., 1., 0.],
                ),
                (
                    [
                        c(-1., -1., -1.),
                        c(1., -1., -1.),
                        c(1., -1., 1.),
                        c(-1., -1., 1.),
                    ],
                    [0., -1., 0.],
                ),
            ];
            for (quad, outward) in quads {
                s.face(&quad, color, Some(outward), false)?;
            }
            Ok(())
        });
        methods.add_method_mut("sphere", |_, s, o: Table| {
            let at = v3(o.raw_get("at")?, "sphere at")?;
            let r = o.raw_get::<Option<f32>>("radius")?.unwrap_or(0.5).abs();
            let color = color_of(&o, Token::Accent)?;
            let detail = o
                .raw_get::<Option<u32>>("detail")?
                .unwrap_or(18)
                .clamp(6, 48);
            let (lon, lat) = (detail * 2, detail);
            let p = |i: u32, j: u32| {
                let theta = std::f32::consts::TAU * i as f32 / lon as f32;
                let phi = std::f32::consts::PI * j as f32 / lat as f32;
                [
                    at[0] + r * phi.sin() * theta.cos(),
                    at[1] + r * phi.cos(),
                    at[2] + r * phi.sin() * theta.sin(),
                ]
            };
            for j in 0..lat {
                for i in 0..lon {
                    let quad = [p(i, j), p(i + 1, j), p(i + 1, j + 1), p(i, j + 1)];
                    let mid = scale(quad.iter().fold([0.0; 3], |a, v| add(a, *v)), 0.25);
                    let corners: Vec<V3> = if j == 0 {
                        vec![quad[0], quad[2], quad[3]]
                    } else if j == lat - 1 {
                        vec![quad[0], quad[1], quad[2]]
                    } else {
                        quad.to_vec()
                    };
                    s.face(&corners, color, Some(sub(mid, at)), false)?;
                }
            }
            Ok(())
        });
        methods.add_method_mut("cylinder", |_, s, o: Table| {
            let at = v3(o.raw_get("at")?, "cylinder at")?;
            let r = o.raw_get::<Option<f32>>("radius")?.unwrap_or(0.5).abs();
            let h = o.raw_get::<Option<f32>>("height")?.unwrap_or(1.0);
            let color = color_of(&o, Token::Accent)?;
            let n = o
                .raw_get::<Option<u32>>("detail")?
                .unwrap_or(24)
                .clamp(6, 64);
            let ring = |i: u32, y: f32| {
                let a = std::f32::consts::TAU * i as f32 / n as f32;
                [at[0] + r * a.cos(), at[1] + y, at[2] + r * a.sin()]
            };
            for i in 0..n {
                let quad = [ring(i, 0.0), ring(i + 1, 0.0), ring(i + 1, h), ring(i, h)];
                let mid = scale(add(quad[0], quad[2]), 0.5);
                s.face(&quad, color, Some(sub(mid, [at[0], mid[1], at[2]])), false)?;
            }
            let top: Vec<V3> = (0..n).map(|i| ring(i, h)).collect();
            let bottom: Vec<V3> = (0..n).map(|i| ring(i, 0.0)).collect();
            s.polygon(&top, color, Some([0.0, h.signum(), 0.0]), false)?;
            s.polygon(&bottom, color, Some([0.0, -h.signum(), 0.0]), false)
        });
        // A height field: y = f(x, z) over a grid, or `values` rows.
        methods.add_method_mut("surface", |_, s, o: Table| {
            let range = |key: &str| -> mlua::Result<(f32, f32)> {
                match o.raw_get::<Option<Table>>(key)? {
                    Some(t) => Ok((t.raw_get::<f32>(1)?, t.raw_get::<f32>(2)?)),
                    None => Ok((-1.0, 1.0)),
                }
            };
            let (x0, x1) = range("x")?;
            let (z0, z1) = range("z")?;
            let colors = colors_of(&o)?;
            let wire = o.raw_get::<Option<bool>>("wire")?.unwrap_or(false);
            let mut grid: Vec<Vec<f32>> = Vec::new();
            if let Some(values) = o.raw_get::<Option<Table>>("values")? {
                for row in values.sequence_values::<Table>() {
                    grid.push(row?.sequence_values::<f32>().collect::<mlua::Result<_>>()?);
                }
            } else {
                let f: Function = o
                    .raw_get::<Option<Function>>("f")?
                    .ok_or_else(|| lua_error("scene:surface needs f = function(x, z) or values"))?;
                let n = o
                    .raw_get::<Option<u32>>("detail")?
                    .unwrap_or(32)
                    .clamp(2, 120) as usize;
                for j in 0..=n {
                    let z = z0 + (z1 - z0) * j as f32 / n as f32;
                    let mut row = Vec::with_capacity(n + 1);
                    for i in 0..=n {
                        let x = x0 + (x1 - x0) * i as f32 / n as f32;
                        row.push(f.call::<f32>((x, z))?);
                    }
                    grid.push(row);
                }
            }
            let rows = grid.len();
            let cols = grid.iter().map(Vec::len).min().unwrap_or(0);
            if rows < 2 || cols < 2 {
                return Err(lua_error("scene:surface needs at least a 2x2 grid"));
            }
            let (lo, hi) = grid
                .iter()
                .flatten()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                    (lo.min(*v), hi.max(*v))
                });
            let span = (hi - lo).max(1e-6);
            let point = |i: usize, j: usize| {
                [
                    x0 + (x1 - x0) * i as f32 / (cols - 1) as f32,
                    grid[j][i],
                    z0 + (z1 - z0) * j as f32 / (rows - 1) as f32,
                ]
            };
            for j in 0..rows - 1 {
                for i in 0..cols - 1 {
                    let quad = [
                        point(i, j),
                        point(i + 1, j),
                        point(i + 1, j + 1),
                        point(i, j + 1),
                    ];
                    let mean = quad.iter().map(|v| v[1]).sum::<f32>() / 4.0;
                    let color = ramp(&colors, (mean - lo) / span);
                    if wire {
                        s.check_items()?;
                        for (a, b) in [(quad[0], quad[1]), (quad[0], quad[3])] {
                            s.lines.push(Line3 {
                                a,
                                b,
                                color,
                                width: 1.0,
                            });
                        }
                    } else {
                        s.face(&quad, color, Some([0.0, 1.0, 0.0]), true)?;
                    }
                }
            }
            Ok(())
        });
        methods.add_method_mut("mesh", |_, s, o: Table| {
            let color = color_of(&o, Token::Accent)?;
            let vertices: Vec<V3> = o
                .raw_get::<Table>("vertices")?
                .sequence_values::<Value>()
                .map(|v| v.and_then(|v| v3(v, "mesh vertex")))
                .collect::<mlua::Result<_>>()?;
            for face in o.raw_get::<Table>("faces")?.sequence_values::<Table>() {
                let corners: Vec<V3> = face?
                    .sequence_values::<usize>()
                    .map(|i| {
                        i.and_then(|i| {
                            vertices.get(i.wrapping_sub(1)).copied().ok_or_else(|| {
                                lua_error(format!("mesh face index {i} is out of range"))
                            })
                        })
                    })
                    .collect::<mlua::Result<_>>()?;
                s.polygon(&corners, color, None, true)?;
            }
            Ok(())
        });
        methods.add_method_mut("line", |_, s, (a, b, o): (Value, Value, Option<Table>)| {
            s.check_items()?;
            let (a, b, o) = match (&a, &b) {
                (Value::Table(t), Value::Nil)
                    if t.raw_get::<Value>("from").is_ok_and(|v| !v.is_nil()) =>
                {
                    (t.raw_get("from")?, t.raw_get("to")?, Some(t.clone()))
                }
                _ => (a, b, o),
            };
            let (color, width) = line_style(o)?;
            s.lines.push(Line3 {
                a: v3(a, "line start")?,
                b: v3(b, "line end")?,
                color,
                width,
            });
            Ok(())
        });
        methods.add_method_mut("path", |_, s, (points, o): (Table, Option<Table>)| {
            let (color, width) = line_style(o)?;
            let pts: Vec<V3> = points
                .sequence_values::<Value>()
                .map(|v| v.and_then(|v| v3(v, "path point")))
                .collect::<mlua::Result<_>>()?;
            for pair in pts.windows(2) {
                s.check_items()?;
                s.lines.push(Line3 {
                    a: pair[0],
                    b: pair[1],
                    color,
                    width,
                });
            }
            Ok(())
        });
        methods.add_method_mut("point", |_, s, (at, o): (Value, Option<Table>)| {
            s.check_items()?;
            let (at, o) = options_form(at, o)?;
            let (color, radius, hint) = match o {
                Some(o) => (
                    color_of(&o, Token::Accent)?,
                    o.raw_get::<Option<f32>>("radius")?
                        .unwrap_or(3.0)
                        .clamp(0.5, 24.0),
                    o.raw_get::<Value>("hint")
                        .map(|v| (!v.is_nil()).then(|| value_text(&v)))?,
                ),
                None => (Color::token(Token::Accent), 3.0, None),
            };
            s.points.push(Point3 {
                at: v3(at, "point")?,
                radius,
                color,
                hint,
            });
            Ok(())
        });
        methods.add_method_mut(
            "label",
            |_, s, (at, text, o): (Value, Value, Option<Table>)| {
                s.check_items()?;
                let (at, text, o) = match (&at, &text) {
                    (Value::Table(t), Value::Nil)
                        if t.raw_get::<Value>("at").is_ok_and(|v| !v.is_nil()) =>
                    {
                        (t.raw_get("at")?, t.raw_get("text")?, Some(t.clone()))
                    }
                    _ => (at, text, o),
                };
                let (color, size) = match o {
                    Some(o) => (
                        color_of(&o, Token::Muted)?,
                        match o.raw_get::<Option<String>>("size")? {
                            Some(size) => {
                                TextSize::parse(&size).map_err(|e| lua_error(e.to_string()))?
                            }
                            None => TextSize::Xs,
                        },
                    ),
                    None => (Color::token(Token::Muted), TextSize::Xs),
                };
                s.labels.push(Label3 {
                    at: v3(at, "label")?,
                    text: value_text(&text),
                    color,
                    size,
                });
                Ok(())
            },
        );
        // A floor grid on y = 0 in hairlines.
        methods.add_method_mut("grid", |_, s, o: Option<Table>| {
            let (size, step, y) = match &o {
                Some(o) => (
                    o.raw_get::<Option<f32>>("size")?
                        .unwrap_or(10.0)
                        .abs()
                        .max(0.1),
                    o.raw_get::<Option<f32>>("step")?
                        .unwrap_or(1.0)
                        .abs()
                        .max(0.01),
                    o.raw_get::<Option<f32>>("y")?.unwrap_or(0.0),
                ),
                None => (10.0, 1.0, 0.0),
            };
            let half = size / 2.0;
            let n = ((size / step).round() as i32).clamp(1, 200);
            for i in 0..=n {
                s.check_items()?;
                let t = -half + size * i as f32 / n as f32;
                let color = Color::token(Token::Border);
                s.lines.push(Line3 {
                    a: [t, y, -half],
                    b: [t, y, half],
                    color,
                    width: 1.0,
                });
                s.lines.push(Line3 {
                    a: [-half, y, t],
                    b: [half, y, t],
                    color,
                    width: 1.0,
                });
            }
            Ok(())
        });
        // x/y/z axes from the origin, with optional names.
        methods.add_method_mut("axes", |_, s, o: Option<Table>| {
            let size = match &o {
                Some(o) => o.raw_get::<Option<f32>>("size")?.unwrap_or(1.0),
                None => 1.0,
            };
            let names: [String; 3] = match &o {
                Some(o) => match o.raw_get::<Option<Table>>("labels")? {
                    Some(t) => [
                        t.raw_get::<Option<String>>(1)?.unwrap_or_default(),
                        t.raw_get::<Option<String>>(2)?.unwrap_or_default(),
                        t.raw_get::<Option<String>>(3)?.unwrap_or_default(),
                    ],
                    None => ["x".into(), "y".into(), "z".into()],
                },
                None => ["x".into(), "y".into(), "z".into()],
            };
            for (axis, name) in names.into_iter().enumerate() {
                s.check_items()?;
                let mut end = [0.0; 3];
                end[axis] = size;
                s.lines.push(Line3 {
                    a: [0.0; 3],
                    b: end,
                    color: Color::token(Token::Muted),
                    width: 1.0,
                });
                if !name.is_empty() {
                    end[axis] = size * 1.08;
                    s.labels.push(Label3 {
                        at: end,
                        text: name,
                        color: Color::token(Token::Muted),
                        size: TextSize::Xs,
                    });
                }
            }
            Ok(())
        });
    }
}

/// `s:point(at, options)` or `s:point { at = ..., ... }`.
fn options_form(at: Value, o: Option<Table>) -> mlua::Result<(Value, Option<Table>)> {
    if let Value::Table(t) = &at
        && o.is_none()
        && let at_field @ Value::Table(_) = t.raw_get::<Value>("at")?
    {
        return Ok((at_field, Some(t.clone())));
    }
    Ok((at, o))
}

fn line_style(o: Option<Table>) -> mlua::Result<(Color, f32)> {
    match o {
        Some(o) => Ok((
            color_of(&o, Token::Muted)?,
            o.raw_get::<Option<f32>>("width")?
                .unwrap_or(1.0)
                .clamp(0.25, 12.0),
        )),
        None => Ok((Color::token(Token::Muted), 1.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> Scene {
        let mut s = Scene::default();
        let h = 0.5;
        let c = |x: f32, y: f32, z: f32| [x * h, y * h, z * h];
        s.face(
            &[
                c(-1., -1., 1.),
                c(1., -1., 1.),
                c(1., 1., 1.),
                c(-1., 1., 1.),
            ],
            Color::token(Token::Accent),
            Some([0., 0., 1.]),
            false,
        )
        .unwrap();
        s.face(
            &[
                c(1., -1., -1.),
                c(-1., -1., -1.),
                c(-1., 1., -1.),
                c(1., 1., -1.),
            ],
            Color::token(Token::Accent),
            Some([0., 0., -1.]),
            false,
        )
        .unwrap();
        s
    }

    #[test]
    fn the_camera_frames_the_geometry_and_culls_back_faces() {
        let projection = cube().project(
            Camera {
                yaw: 0.0,
                pitch: 0.0,
                zoom: 1.0,
            },
            400.0,
            300.0,
        );
        // Looking down -z from +z: only the +z face is drawn.
        assert_eq!(projection.prims.len(), 1);
        let Prim::Face { points, shade, .. } = &projection.prims[0] else {
            panic!()
        };
        for [x, y] in points {
            assert!(
                (0.0..=400.0).contains(x) && (0.0..=300.0).contains(y),
                "{x},{y}"
            );
        }
        assert!(*shade > 0.35);
        // From behind, the other face.
        let behind = cube().project(
            Camera {
                yaw: std::f32::consts::PI,
                pitch: 0.0,
                zoom: 1.0,
            },
            400.0,
            300.0,
        );
        assert_eq!(behind.prims.len(), 1);
    }

    #[test]
    fn faces_sort_back_to_front() {
        let mut s = Scene::default();
        let near = [[0., 0., 1.], [1., 0., 1.], [1., 1., 1.]];
        let far = [[0., 0., -1.], [1., 0., -1.], [1., 1., -1.]];
        s.face(&near, Color::token(Token::Chart(0)), None, true)
            .unwrap();
        s.face(&far, Color::token(Token::Chart(1)), None, true)
            .unwrap();
        let p = s.project(
            Camera {
                yaw: 0.0,
                pitch: 0.0,
                zoom: 1.0,
            },
            200.0,
            200.0,
        );
        let colors: Vec<Token> = p
            .prims
            .iter()
            .map(|p| match p {
                Prim::Face { color, .. } => color.token,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(colors, [Token::Chart(1), Token::Chart(0)]);
    }

    #[test]
    fn ramps_mix_between_neighbouring_tokens() {
        let colors = [
            Color::token(Token::Chart(0)),
            Color::token(Token::Chart(1)),
            Color::token(Token::Chart(2)),
        ];
        let mid = ramp(&colors, 0.75);
        assert_eq!(mid.token, Token::Chart(1));
        assert_eq!(mid.mix.unwrap().0, Token::Chart(2));
        assert!((mid.mix.unwrap().1 - 0.5).abs() < 1e-5);
    }
}
