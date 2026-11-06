//! A small, honest 3D renderer: perspective, a depth buffer, Lambert shading, and a PNG.
//!
//! The demo has no GPU dependency for the same reason it has no window: an artefact nobody can
//! regenerate is not evidence. This rasteriser is what makes the 3D renders reproducible — the same
//! city, the same meshes and the same seed produce the same pixels on any machine, because every
//! operation is `f32` arithmetic in a fixed order and nothing samples a texture.
//!
//! What it is not: a replacement for Bevy's renderer. There is no PBR, no shadows, no MSAA beyond a
//! half-pixel jitter, no texture sampling and no HDR. It draws **the same scene the engine would
//! draw** — `Scene::to_json` is the contract — from the same meshes, which is what a technical demo
//! owes a reader: a picture whose provenance is the code beside it.

use crate::jsonr;
use bevy_math::{Mat4, Vec3, Vec4};

/// A rendered image: RGB8 plus the depth buffer that produced it.
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    colour: Vec<[u8; 3]>,
    depth: Vec<f32>,
}

impl Image {
    /// A black image with every depth set to the far plane.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            colour: vec![[0, 0, 0]; (width as usize) * (height as usize)],
            depth: vec![1.0; (width as usize) * (height as usize)],
        }
    }

    /// Fill with a vertical gradient, which is the sky.
    pub fn sky(&mut self, top: [f32; 3], horizon: [f32; 3]) {
        self.sky_to(top, horizon, self.height.saturating_sub(1));
    }

    /// Fill the sky, putting the horizon colour on `horizon_row` and below.
    ///
    /// The row matters: a gradient that only reaches its horizon colour at the *bottom* of the
    /// image leaves the rows just above the horizon much darker than the fogged ground, and the far
    /// edge of the ground plane then reads as a bright wedge across the frame. Placing the horizon
    /// where the camera actually puts it makes the plane dissolve instead of ending.
    pub fn sky_to(&mut self, top: [f32; 3], horizon: [f32; 3], horizon_row: u32) {
        let horizon_row = horizon_row.min(self.height.saturating_sub(1));
        let span = horizon_row.max(1) as f32;
        for y in 0..self.height {
            let t = if y <= horizon_row {
                (y as f32 / span).powf(1.35)
            } else {
                1.0
            };
            let colour = [
                top[0] + (horizon[0] - top[0]) * t,
                top[1] + (horizon[1] - top[1]) * t,
                top[2] + (horizon[2] - top[2]) * t,
            ];
            let encoded = encode(colour);
            let row = (y as usize) * (self.width as usize);
            for x in 0..self.width as usize {
                if let Some(slot) = self.colour.get_mut(row + x) {
                    *slot = encoded;
                }
            }
        }
    }

    /// The pixel at `(x, y)`, or black outside the image.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let index = (y as usize) * (self.width as usize) + (x as usize);
        self.colour.get(index).copied().unwrap_or([0, 0, 0])
    }

    /// A fingerprint of the pixels, for the run report and the determinism test.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for pixel in &self.colour {
            for channel in pixel {
                hash ^= u64::from(*channel);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        hash
    }

    /// Box-filter the image down by an integer factor.
    ///
    /// This is the cheapest antialiasing a rasteriser with no coverage samples can do: render at
    /// twice the size and average four taps. A kerb 0.3 m wide is sub-pixel at a hundred metres,
    /// and without this it breaks into a dotted line instead of a line.
    #[must_use]
    pub fn downsample(&self, factor: u32) -> Self {
        let factor = factor.max(1);
        if factor == 1 {
            return Self {
                width: self.width,
                height: self.height,
                colour: self.colour.clone(),
                depth: self.depth.clone(),
            };
        }
        let width = (self.width / factor).max(1);
        let height = (self.height / factor).max(1);
        let mut out = Self::new(width, height);
        let taps = (factor * factor) as f32;
        for y in 0..height {
            for x in 0..width {
                let mut sum = [0.0_f32; 3];
                for sub_y in 0..factor {
                    for sub_x in 0..factor {
                        let pixel = self.pixel(x * factor + sub_x, y * factor + sub_y);
                        for (axis, value) in pixel.iter().enumerate() {
                            if let Some(slot) = sum.get_mut(axis) {
                                *slot += f32::from(*value);
                            }
                        }
                    }
                }
                let averaged = [
                    (sum[0] / taps).round().clamp(0.0, 255.0) as u8,
                    (sum[1] / taps).round().clamp(0.0, 255.0) as u8,
                    (sum[2] / taps).round().clamp(0.0, 255.0) as u8,
                ];
                let index = (y as usize) * (width as usize) + (x as usize);
                if let Some(slot) = out.colour.get_mut(index) {
                    *slot = averaged;
                }
            }
        }
        out
    }

    /// How many pixels are not the sky, as a cheap "did anything draw" check.
    #[must_use]
    pub fn coverage(&self) -> f32 {
        let drawn = self.depth.iter().filter(|depth| **depth < 1.0).count();
        drawn as f32 / self.depth.len().max(1) as f32
    }

    fn plot(&mut self, x: usize, y: usize, depth: f32, colour: [u8; 3]) {
        let index = y * (self.width as usize) + x;
        let Some(slot) = self.depth.get_mut(index) else {
            return;
        };
        if depth >= *slot {
            return;
        }
        *slot = depth;
        if let Some(pixel) = self.colour.get_mut(index) {
            *pixel = colour;
        }
    }

    /// Encode as a PNG.
    ///
    /// No `png` crate: the format is a signature, an `IHDR`, one `IDAT` of zlib-compressed scanlines
    /// and an `IEND`, and the compressor below is a fixed-Huffman deflate with an LZ77 match finder.
    /// That is a page of arithmetic against a dependency the demo would carry forever.
    #[must_use]
    pub fn to_png(&self) -> Vec<u8> {
        let mut raw = Vec::with_capacity((self.width as usize) * (self.height as usize) * 3 + 1);
        for y in 0..self.height as usize {
            raw.push(0); // filter: none. The images are smooth enough that a filter buys little.
            let row = y * (self.width as usize);
            for x in 0..self.width as usize {
                raw.extend_from_slice(
                    self.colour
                        .get(row + x)
                        .copied()
                        .unwrap_or([0, 0, 0])
                        .as_slice_of(),
                );
            }
        }
        let compressed = zlib(&raw);

        let mut png = Vec::with_capacity(compressed.len() + 64);
        png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        let mut header = Vec::with_capacity(13);
        header.extend_from_slice(&self.width.to_be_bytes());
        header.extend_from_slice(&self.height.to_be_bytes());
        header.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut png, *b"IHDR", &header);
        chunk(&mut png, *b"IDAT", &compressed);
        chunk(&mut png, *b"IEND", &[]);
        png
    }
}

/// `[u8; 3]` as a slice, for extending a buffer.
trait Slice3 {
    fn as_slice_of(&self) -> &[u8];
}

impl Slice3 for [u8; 3] {
    fn as_slice_of(&self) -> &[u8] {
        self
    }
}

fn chunk(out: &mut Vec<u8>, kind: [u8; 4], body: &[u8]) {
    out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(0).to_be_bytes());
    out.extend_from_slice(&kind);
    out.extend_from_slice(body);
    let mut crc = Crc::new();
    crc.update(&kind);
    crc.update(body);
    out.extend_from_slice(&crc.finish().to_be_bytes());
}

/// CRC-32, which PNG uses on every chunk.
struct Crc {
    table: [u32; 256],
    value: u32,
}

impl Crc {
    fn new() -> Self {
        let mut table = [0_u32; 256];
        for (index, slot) in table.iter_mut().enumerate() {
            let mut value = u32::try_from(index).unwrap_or(0);
            for _ in 0..8 {
                value = if value & 1 == 1 {
                    0xedb8_8320 ^ (value >> 1)
                } else {
                    value >> 1
                };
            }
            *slot = value;
        }
        Self {
            table,
            value: 0xffff_ffff,
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            let index = ((self.value ^ u32::from(*byte)) & 0xff) as usize;
            self.value = self.table.get(index).copied().unwrap_or(0) ^ (self.value >> 8);
        }
    }

    fn finish(&self) -> u32 {
        self.value ^ 0xffff_ffff
    }
}

/// zlib-wrap a fixed-Huffman deflate stream.
pub fn zlib(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() / 3);
    out.extend_from_slice(&[0x78, 0x01]); // deflate, 32K window, fastest
    deflate(raw, &mut out);
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

fn adler32(bytes: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for byte in bytes {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

/// Bit writer, least-significant bit first, as deflate packs its stream.
struct Bits {
    bytes: Vec<u8>,
    accumulator: u32,
    count: u32,
}

impl Bits {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            accumulator: 0,
            count: 0,
        }
    }

    fn push(&mut self, value: u32, bits: u32) {
        self.accumulator |= (value & ((1 << bits) - 1)) << self.count;
        self.count += bits;
        while self.count >= 8 {
            self.bytes.push((self.accumulator & 0xff) as u8);
            self.accumulator >>= 8;
            self.count -= 8;
        }
    }

    /// Huffman codes are defined most-significant bit first even though the stream is packed least
    /// significant bit first, so a code goes in reversed.
    fn huffman(&mut self, code: u32, bits: u32) {
        let mut reversed = 0_u32;
        for bit in 0..bits {
            reversed |= ((code >> bit) & 1) << (bits - 1 - bit);
        }
        self.push(reversed, bits);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            self.bytes.push((self.accumulator & 0xff) as u8);
        }
        self.bytes
    }
}

/// Length codes: (base length, extra bits) for codes 257..=285.
const LENGTHS: [(u32, u32); 29] = [
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 0),
    (7, 0),
    (8, 0),
    (9, 0),
    (10, 0),
    (11, 1),
    (13, 1),
    (15, 1),
    (17, 1),
    (19, 2),
    (23, 2),
    (27, 2),
    (31, 2),
    (35, 3),
    (43, 3),
    (51, 3),
    (59, 3),
    (67, 4),
    (83, 4),
    (99, 4),
    (115, 4),
    (131, 5),
    (163, 5),
    (195, 5),
    (227, 5),
    (258, 0),
];

/// Distance codes: (base distance, extra bits) for codes 0..=29.
const DISTANCES: [(u32, u32); 30] = [
    (1, 0),
    (2, 0),
    (3, 0),
    (4, 0),
    (5, 1),
    (7, 1),
    (9, 2),
    (13, 2),
    (17, 3),
    (25, 3),
    (33, 4),
    (49, 4),
    (65, 5),
    (97, 5),
    (129, 6),
    (193, 6),
    (257, 7),
    (385, 7),
    (513, 8),
    (769, 8),
    (1025, 9),
    (1537, 9),
    (2049, 10),
    (3073, 10),
    (4097, 11),
    (6145, 11),
    (8193, 12),
    (12289, 12),
    (16385, 13),
    (24577, 13),
];

fn literal(bits: &mut Bits, symbol: u32) {
    match symbol {
        0..=143 => bits.huffman(0x30 + symbol, 8),
        144..=255 => bits.huffman(0x190 + (symbol - 144), 9),
        256..=279 => bits.huffman(symbol - 256, 7),
        _ => bits.huffman(0xc0 + (symbol - 280), 8),
    }
}

fn length_symbol(length: u32) -> (u32, u32, u32) {
    let mut symbol = 28;
    for (index, (base, extra)) in LENGTHS.iter().enumerate() {
        if length < *base {
            symbol = index.saturating_sub(1);
            break;
        }
        symbol = index;
        let _ = extra;
    }
    let (base, extra) = LENGTHS.get(symbol).copied().unwrap_or((3, 0));
    (257 + symbol as u32, length - base, extra)
}

fn distance_symbol(distance: u32) -> (u32, u32, u32) {
    let mut symbol = 29;
    for (index, (base, _)) in DISTANCES.iter().enumerate() {
        if distance < *base {
            symbol = index.saturating_sub(1);
            break;
        }
        symbol = index;
    }
    let (base, extra) = DISTANCES.get(symbol).copied().unwrap_or((1, 0));
    (symbol as u32, distance - base, extra)
}

/// One fixed-Huffman deflate block over LZ77 matches.
fn deflate(raw: &[u8], out: &mut Vec<u8>) {
    const WINDOW: usize = 32_768;
    const HASH_BITS: usize = 15;
    const MAX_MATCH: usize = 258;
    const MAX_CHAIN: usize = 24;

    let mut bits = Bits::new();
    bits.push(1, 1); // BFINAL
    bits.push(1, 2); // BTYPE = fixed Huffman

    let mut head = vec![u32::MAX; 1 << HASH_BITS];
    let mut prev = vec![u32::MAX; raw.len().max(1)];

    let hash = |at: usize| -> usize {
        let a = u32::from(raw.get(at).copied().unwrap_or(0));
        let b = u32::from(raw.get(at + 1).copied().unwrap_or(0));
        let c = u32::from(raw.get(at + 2).copied().unwrap_or(0));
        (((a << 10) ^ (b << 5) ^ c) & ((1 << HASH_BITS) - 1)) as usize
    };

    let mut at = 0_usize;
    while at < raw.len() {
        let mut best_length = 0_usize;
        let mut best_distance = 0_usize;
        if at + 3 <= raw.len() {
            let mut candidate = head.get(hash(at)).copied().unwrap_or(u32::MAX);
            let mut chain = 0;
            while candidate != u32::MAX && chain < MAX_CHAIN {
                let start = candidate as usize;
                if at.saturating_sub(start) > WINDOW {
                    break;
                }
                let mut length = 0;
                while length < MAX_MATCH
                    && at + length < raw.len()
                    && raw.get(start + length) == raw.get(at + length)
                {
                    length += 1;
                }
                if length > best_length {
                    best_length = length;
                    best_distance = at - start;
                    if length >= MAX_MATCH {
                        break;
                    }
                }
                candidate = prev.get(start).copied().unwrap_or(u32::MAX);
                chain += 1;
            }
        }

        if best_length >= 3 {
            let (code, extra_value, extra_bits) = length_symbol(best_length as u32);
            literal(&mut bits, code);
            bits.push(extra_value, extra_bits);
            let (code, extra_value, extra_bits) = distance_symbol(best_distance as u32);
            bits.huffman(code, 5);
            bits.push(extra_value, extra_bits);
            for step in 0..best_length {
                let position = at + step;
                if position + 3 <= raw.len() {
                    let slot = hash(position);
                    if let Some(previous) = head.get_mut(slot) {
                        if let Some(chain) = prev.get_mut(position) {
                            *chain = *previous;
                        }
                        *previous = u32::try_from(position).unwrap_or(u32::MAX);
                    }
                }
            }
            at += best_length;
        } else {
            literal(&mut bits, u32::from(raw.get(at).copied().unwrap_or(0)));
            if at + 3 <= raw.len() {
                let slot = hash(at);
                if let Some(previous) = head.get_mut(slot) {
                    if let Some(chain) = prev.get_mut(at) {
                        *chain = *previous;
                    }
                    *previous = u32::try_from(at).unwrap_or(u32::MAX);
                }
            }
            at += 1;
        }
    }
    literal(&mut bits, 256);
    out.extend_from_slice(&bits.finish());
}

/// Convert a linear-ish colour to 8-bit sRGB, clamping.
fn encode(colour: [f32; 3]) -> [u8; 3] {
    let channel = |value: f32| -> u8 {
        let clamped = value.clamp(0.0, 1.0);
        // A light gamma makes the shading read on paper; the scene is not physically lit.
        let shaped = clamped.powf(1.0 / 2.2);
        (shaped * 255.0 + 0.5) as u8
    };
    [channel(colour[0]), channel(colour[1]), channel(colour[2])]
}

/// How a triangle's surface responds to light.
#[derive(Debug, Clone, Copy)]
pub enum Style {
    /// A lit surface: Lambert plus a hemispheric ambient and a rim.
    Lit,
    /// A ground plane with a grid every `cell` metres.
    Ground {
        /// Grid spacing, metres.
        cell: f32,
    },
    /// A building face with floor bands every `storey` metres.
    Facade {
        /// Floor height, metres.
        storey: f32,
    },
}

/// A vertex, after the model matrix.
#[derive(Debug, Clone, Copy)]
pub struct Vertex {
    /// World position.
    pub world: Vec3,
    /// World normal.
    pub normal: Vec3,
}

/// The lights and atmosphere a frame is drawn with.
#[derive(Debug, Clone, Copy)]
pub struct Atmosphere {
    /// Direction **towards** the key light, normalised, so a sun above the scene has `y > 0`.
    ///
    /// This is the opposite convention from a "light direction" that points at what it illuminates,
    /// and it was the bug in the first render of the Ashfall scene: the ground has a normal of `+Y`,
    /// the shading term was `(-sun) · n`, and a sun with `y > 0` therefore lit the ground with a
    /// negative Lambert term — no light at all, and a plane of flat grey.
    pub sun: Vec3,
    /// Key light colour.
    pub sun_colour: [f32; 3],
    /// Sky colour, used for the upper hemisphere ambient.
    pub sky: [f32; 3],
    /// Ground bounce colour, used for the lower hemisphere ambient.
    pub bounce: [f32; 3],
    /// Distance where the fog starts, metres.
    pub fog_near: f32,
    /// Distance where the fog is total, metres.
    pub fog_far: f32,
    /// Fog colour, which should match the horizon.
    pub fog: [f32; 3],
}

impl Default for Atmosphere {
    fn default() -> Self {
        Self {
            sun: Vec3::new(-0.45, 0.78, 0.44).normalize(),
            sun_colour: [1.06, 0.99, 0.88],
            // Linear-light ambient. The albedos in `render3d` are linear too, and the display
            // transform is a 1/2.2 curve, so a sunlit ground at 0.24 linear lands near 0.47 on
            // screen — mid grey. The first pass used sRGB-looking values as if they were linear
            // and produced a washed-out plane at 0.85.
            sky: [0.13, 0.17, 0.24],
            bounce: [0.05, 0.05, 0.05],
            fog_near: 90.0,
            fog_far: 620.0,
            fog: [0.62, 0.58, 0.54],
        }
    }
}

/// A perspective camera.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    /// Where the eye is, metres.
    pub eye: Vec3,
    /// What it looks at.
    pub target: Vec3,
    /// Vertical field of view, radians.
    pub fov_y: f32,
    /// Near plane, metres.
    pub near: f32,
    /// Far plane, metres.
    pub far: f32,
}

impl Camera {
    /// The view-projection matrix for an image of this aspect ratio.
    #[must_use]
    pub fn view_projection(&self, aspect: f32) -> Mat4 {
        // Written out rather than taken from `glam`'s helpers: the right-handed look-at and
        // perspective constructors are deprecated in the pinned version, and a rasteriser that
        // depends on a deprecated matrix is one upgrade away from a silently flipped image.
        let eye = self.eye;
        let forward = (self.target - eye).normalize_or_zero();
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(forward);
        let view = Mat4::from_cols(
            Vec4::new(right.x, up.x, -forward.x, 0.0),
            Vec4::new(right.y, up.y, -forward.y, 0.0),
            Vec4::new(right.z, up.z, -forward.z, 0.0),
            Vec4::new(-right.dot(eye), -up.dot(eye), forward.dot(eye), 1.0),
        );
        let aspect = aspect.max(0.01);
        let f = 1.0 / (self.fov_y * 0.5).tan();
        let near = self.near;
        let far = self.far;
        let projection = Mat4::from_cols(
            Vec4::new(f / aspect, 0.0, 0.0, 0.0),
            Vec4::new(0.0, f, 0.0, 0.0),
            Vec4::new(0.0, 0.0, (far + near) / (near - far), -1.0),
            Vec4::new(0.0, 0.0, 2.0 * far * near / (near - far), 0.0),
        );
        projection * view
    }
}

/// Draws triangles into an [`Image`].
pub struct Renderer {
    view_projection: Mat4,
    eye: Vec3,
    fov_y: f32,
    atmosphere: Atmosphere,
}

/// One triangle ready to rasterise, in world space.
pub struct Face {
    /// Its three vertices.
    pub vertices: [Vertex; 3],
    /// Its albedo, sRGB.
    pub albedo: [f32; 3],
    /// How it shades.
    pub style: Style,
}

impl Renderer {
    /// A renderer for one frame.
    #[must_use]
    pub fn new(camera: &Camera, aspect: f32, atmosphere: Atmosphere) -> Self {
        Self {
            view_projection: camera.view_projection(aspect),
            eye: camera.eye,
            fov_y: camera.fov_y,
            atmosphere,
        }
    }

    /// Radians per pixel across one pixel of a raster of the given height, which is the world-space
    /// size of a pixel one metre away.
    fn pixel_angle(&self, height: u32) -> f32 {
        let height = f32::from(u16::try_from(height).unwrap_or(u16::MAX)).max(2.0);
        2.0 * (self.fov_y * 0.5).tan() / height
    }

    /// Rasterise one triangle, clipping it against the near plane first.
    pub fn triangle(&self, image: &mut Image, face: &Face) {
        // Clip space, then clip against z >= 0, which is the near plane for a 0..1 depth range.
        let mut polygon: Vec<([f32; 4], Vertex)> = Vec::with_capacity(4);
        let positions: Vec<([f32; 4], Vertex)> = face
            .vertices
            .iter()
            .map(|vertex| {
                let clip = self.view_projection * vertex.world.extend(1.0);
                (clip.to_array(), *vertex)
            })
            .collect();

        for index in 0..positions.len() {
            let current = positions.get(index).copied();
            let next = positions.get((index + 1) % positions.len()).copied();
            let (Some((current_clip, current_vertex)), Some((next_clip, next_vertex))) =
                (current, next)
            else {
                continue;
            };
            let current_z = current_clip.get(2).copied().unwrap_or(-1.0);
            let next_z = next_clip.get(2).copied().unwrap_or(-1.0);
            let current_in = current_z >= 1.0e-4;
            let next_in = next_z >= 1.0e-4;
            if current_in {
                polygon.push((current_clip, current_vertex));
            }
            if current_in != next_in {
                let denominator = next_z - current_z;
                let t = if denominator.abs() < 1.0e-9 {
                    0.0
                } else {
                    (1.0e-4 - current_z) / denominator
                };
                let t = t.clamp(0.0, 1.0);
                let mixed = mix4(current_clip, next_clip, t);
                polygon.push((
                    mixed,
                    Vertex {
                        world: current_vertex.world.lerp(next_vertex.world, t),
                        normal: current_vertex
                            .normal
                            .lerp(next_vertex.normal, t)
                            .normalize_or_zero(),
                    },
                ));
            }
        }

        if polygon.len() < 3 {
            return;
        }
        for index in 1..polygon.len() - 1 {
            let (Some(a), Some(b), Some(c)) = (
                polygon.first().copied(),
                polygon.get(index).copied(),
                polygon.get(index + 1).copied(),
            ) else {
                continue;
            };
            self.raster(image, [a, b, c], face.albedo, face.style);
        }
    }

    fn raster(
        &self,
        image: &mut Image,
        triangle: [([f32; 4], Vertex); 3],
        albedo: [f32; 3],
        style: Style,
    ) {
        let mut screen = [(0.0_f32, 0.0_f32, 0.0_f32, 0.0_f32); 3]; // x, y, depth, 1/w
        for (index, (clip, _)) in triangle.iter().enumerate() {
            let w = clip.get(3).copied().unwrap_or(1.0);
            if w <= 1.0e-6 {
                return;
            }
            let x = clip.first().copied().unwrap_or(0.0) / w;
            let y = clip.get(1).copied().unwrap_or(0.0) / w;
            let z = clip.get(2).copied().unwrap_or(0.0) / w;
            let sx = (x * 0.5 + 0.5) * image.width as f32;
            // Screen y grows downward, NDC y grows upward.
            let sy = (1.0 - (y * 0.5 + 0.5)) * image.height as f32;
            if let Some(slot) = screen.get_mut(index) {
                *slot = (sx, sy, z, 1.0 / w);
            }
        }
        let (Some(a), Some(b), Some(c)) = (
            screen.first().copied(),
            screen.get(1).copied(),
            screen.get(2).copied(),
        ) else {
            return;
        };
        let area = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
        if area.abs() < 1.0e-6 {
            return;
        }
        let min_x = a.0.min(b.0).min(c.0).floor().max(0.0) as usize;
        let max_x = (a.0.max(b.0).max(c.0).ceil() as usize).min(image.width as usize);
        let min_y = a.1.min(b.1).min(c.1).floor().max(0.0) as usize;
        let max_y = (a.1.max(b.1).max(c.1).ceil() as usize).min(image.height as usize);
        if min_x >= max_x || min_y >= max_y {
            return;
        }
        let inverse_area = 1.0 / area;
        for py in min_y..max_y {
            let sample_y = py as f32 + 0.5;
            for px in min_x..max_x {
                let sample_x = px as f32 + 0.5;
                let w0 = ((b.0 - sample_x) * (c.1 - sample_y)
                    - (b.1 - sample_y) * (c.0 - sample_x))
                    * inverse_area;
                let w1 = ((c.0 - sample_x) * (a.1 - sample_y)
                    - (c.1 - sample_y) * (a.0 - sample_x))
                    * inverse_area;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let depth = w0 * a.2 + w1 * b.2 + w2 * c.2;
                if !(0.0..=1.0).contains(&depth) {
                    continue;
                }
                // Perspective-correct attribute weights.
                let w_a = w0 * a.3;
                let w_b = w1 * b.3;
                let w_c = w2 * c.3;
                let total = w_a + w_b + w_c;
                if total.abs() < 1.0e-9 {
                    continue;
                }
                let (Some(normal_a), Some(normal_b), Some(normal_c)) = (
                    triangle.first().map(|(_, vertex)| vertex.normal),
                    triangle.get(1).map(|(_, vertex)| vertex.normal),
                    triangle.get(2).map(|(_, vertex)| vertex.normal),
                ) else {
                    continue;
                };
                let (Some(world_a), Some(world_b), Some(world_c)) = (
                    triangle.first().map(|(_, vertex)| vertex.world),
                    triangle.get(1).map(|(_, vertex)| vertex.world),
                    triangle.get(2).map(|(_, vertex)| vertex.world),
                ) else {
                    continue;
                };
                let normal = (normal_a * (w_a / total)
                    + normal_b * (w_b / total)
                    + normal_c * (w_c / total))
                    .normalize_or_zero();
                let world =
                    world_a * (w_a / total) + world_b * (w_b / total) + world_c * (w_c / total);
                let incident = self.eye - world;
                let distance = incident.length();
                let view = if distance > 1.0e-6 {
                    incident / distance
                } else {
                    Vec3::Y
                };
                let footprint = distance * self.pixel_angle(image.height);
                let shade = self.shade(normal, view, albedo, style, world, footprint);
                let fog = ((distance - self.atmosphere.fog_near)
                    / (self.atmosphere.fog_far - self.atmosphere.fog_near).max(1.0))
                .clamp(0.0, 1.0);
                let fog = fog * fog * (3.0 - 2.0 * fog);
                let colour = [
                    shade[0] + (self.atmosphere.fog[0] - shade[0]) * fog,
                    shade[1] + (self.atmosphere.fog[1] - shade[1]) * fog,
                    shade[2] + (self.atmosphere.fog[2] - shade[2]) * fog,
                ];
                image.plot(px, py, depth, encode(colour));
            }
        }
    }

    fn shade(
        &self,
        normal: Vec3,
        view: Vec3,
        albedo: [f32; 3],
        style: Style,
        world: Vec3,
        footprint: f32,
    ) -> [f32; 3] {
        let normal = if normal.length_squared() < 1.0e-6 {
            Vec3::Y
        } else {
            normal
        };
        let lambert = self.atmosphere.sun.dot(normal).max(0.0);
        let hemisphere = normal.y * 0.5 + 0.5;
        let mut ambient = [0.0_f32; 3];
        for (axis, slot) in ambient.iter_mut().enumerate() {
            let sky = self.atmosphere.sky.get(axis).copied().unwrap_or(0.5);
            let bounce = self.atmosphere.bounce.get(axis).copied().unwrap_or(0.2);
            *slot = bounce + (sky - bounce) * hemisphere;
        }
        // The rim term belongs on silhouettes, not on a ground plane seen at a grazing angle,
        // where every pixel is a silhouette and the whole plane would gain 0.16 of white.
        let rim = match style {
            Style::Ground { .. } => 0.0,
            Style::Lit | Style::Facade { .. } => {
                (1.0 - view.dot(normal).clamp(0.0, 1.0)).powi(3) * 0.16
            }
        };

        let mut detail = 1.0_f32;
        match style {
            Style::Lit => {}
            Style::Ground { cell } => {
                // Streets, drawn with a coverage term rather than a threshold. A world-space line
                // thinner than a pixel is a sampling test, and along a shallow plane the test's
                // answer flips in bands: that is what drew a dotted caterpillar where a kerb
                // belongs. Widening the line to the pixel footprint and fading its contrast as the
                // footprint grows averages instead of flickering.
                let line = |value: f32| {
                    let phase = (value / cell).fract();
                    let distance = (phase.min(1.0 - phase) * cell).abs();
                    let width = (0.28 + footprint * 0.6).min(cell * 0.5).max(1.0e-3);
                    // Energy-preserving: a 0.28 m line spread over a `width` footprint covers
                    // `0.28/width` of the pixel, so the band gets broader and fainter with
                    // distance instead of becoming a smear of ink.
                    (1.0 - (distance / width)).clamp(0.0, 1.0) * (0.28 / width).min(1.0)
                };
                detail = 1.0 - 0.48 * line(world.x).max(line(world.z));
            }
            Style::Facade { storey } => {
                // A floor band and a window rhythm, both faded by the pixel footprint so a facade
                // two hundred metres away is a tone rather than a shimmer.
                let floor = (world.y / storey).fract();
                let band = (1.0 - (floor / (0.14 + footprint * 0.5)).clamp(0.0, 1.0)).powi(2);
                let across = if normal.x.abs() > normal.z.abs() {
                    world.z
                } else {
                    world.x
                };
                let phase = (across / 3.2).fract();
                let spread = (0.42 + footprint * 0.7).max(1.0e-3);
                let mullion =
                    (1.0 - ((phase.min(1.0 - phase) * 3.2) / spread).clamp(0.0, 1.0)).powi(2);
                let window_row = if (0.24..0.78).contains(&floor) {
                    1.0
                } else {
                    0.0
                };
                detail = 1.0 - 0.26 * band - 0.22 * window_row * mullion;
            }
        }
        let mut out = [0.0_f32; 3];
        for (axis, slot) in out.iter_mut().enumerate() {
            let base = albedo.get(axis).copied().unwrap_or(0.5);
            let sun = self.atmosphere.sun_colour.get(axis).copied().unwrap_or(1.0);
            let ambient = ambient.get(axis).copied().unwrap_or(0.0);
            *slot = base * (ambient + sun * lambert * 0.95 + rim) * detail;
        }
        out
    }
}

fn mix4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

/// Parse a `#rrggbb` colour, for the shot table.
#[must_use]
pub fn colour(hex: &str) -> [f32; 3] {
    let digits = hex.trim_start_matches('#');
    let channel = |index: usize| -> f32 {
        let slice = digits.get(index * 2..index * 2 + 2).unwrap_or("80");
        u8::from_str_radix(slice, 16).map_or(0.5, |value| f32::from(value) / 255.0)
    };
    [channel(0), channel(1), channel(2)]
}

/// Read a colour out of a JSON document, defaulting when it is absent.
#[must_use]
pub fn json_colour(value: Option<&jsonr::Value>, fallback: [f32; 3]) -> [f32; 3] {
    value.and_then(jsonr::Value::vec3).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_flock::testkit::OrDie;

    /// Inflate a fixed-Huffman deflate stream, which is all the compressor emits.
    ///
    /// Written from RFC 1951 rather than shared with the compressor, so a desynchronised bit order
    /// in either direction fails the test instead of agreeing with itself.
    fn inflate_fixed(stream: &[u8]) -> Vec<u8> {
        struct Reader<'a> {
            bytes: &'a [u8],
            at: usize,
            bit: u32,
        }
        impl Reader<'_> {
            fn bit(&mut self) -> u32 {
                let byte = self.bytes.get(self.at).copied().unwrap_or(0);
                let value = u32::from((byte >> self.bit) & 1);
                self.bit += 1;
                if self.bit == 8 {
                    self.bit = 0;
                    self.at += 1;
                }
                value
            }
            fn bits(&mut self, count: u32) -> u32 {
                let mut value = 0;
                for index in 0..count {
                    value |= self.bit() << index;
                }
                value
            }
            /// Read `count` bits most-significant first, which is how a Huffman code is packed.
            fn msb(&mut self, count: u32) -> u32 {
                let mut value = 0;
                for _ in 0..count {
                    value = (value << 1) | self.bit();
                }
                value
            }

            fn huffman(&mut self) -> u32 {
                let mut code = 0_u32;
                for length in 1..=15 {
                    code = (code << 1) | self.bit();
                    if let Some(symbol) = decode_fixed(code, length) {
                        return symbol;
                    }
                }
                256
            }
        }
        let mut reader = Reader {
            bytes: stream,
            at: 0,
            bit: 0,
        };
        let final_block = reader.bit();
        let kind = reader.bits(2);
        assert_eq!(final_block, 1, "expected a single final block");
        assert_eq!(kind, 1, "expected a fixed-Huffman block");
        let mut out: Vec<u8> = Vec::new();
        loop {
            let symbol = reader.huffman();
            match symbol {
                0..=255 => out.push(symbol as u8),
                256 => break,
                length_symbol => {
                    let (base, extra) = LENGTHS
                        .get((length_symbol - 257) as usize)
                        .copied()
                        .unwrap_or((3, 0));
                    let length = base + reader.bits(extra);
                    // Distance codes are five-bit Huffman codes: their value is the symbol, and
                    // like every Huffman code they arrive most-significant bit first.
                    let distance_code = reader.msb(5);
                    let (base, extra) = DISTANCES
                        .get(distance_code as usize)
                        .copied()
                        .unwrap_or((1, 0));
                    let distance = base + reader.bits(extra);
                    let start = out.len().saturating_sub(distance as usize);
                    for step in 0..length as usize {
                        let byte = out.get(start + step).copied().unwrap_or(0);
                        out.push(byte);
                    }
                }
            }
        }
        out
    }

    fn decode_fixed(code: u32, length: u32) -> Option<u32> {
        match (length, code) {
            (7, 0x00..=0x17) => Some(256 + code),
            (8, 0x30..=0xbf) => Some(code - 0x30),
            (8, 0xc0..=0xc7) => Some(280 + code - 0xc0),
            (9, 0x190..=0x1ff) => Some(144 + code - 0x190),
            _ => None,
        }
    }

    #[test]
    fn the_compressor_round_trips() {
        let mut raw = Vec::new();
        for index in 0..4096 {
            raw.push((index % 251) as u8);
        }
        raw.extend(std::iter::repeat_n(7_u8, 2000));
        let compressed = zlib(&raw);
        assert_eq!(compressed.first().copied(), Some(0x78));
        let Some(payload) = compressed.get(2..compressed.len().saturating_sub(4)) else {
            return;
        };
        let inflated = inflate_fixed(payload);
        assert_eq!(
            inflated.len(),
            raw.len(),
            "the decoder produced a different length"
        );
        let divergence = (0..raw.len()).find(|index| inflated.get(*index) != raw.get(*index));
        assert!(
            divergence.is_none(),
            "the decoder diverged at {:?}: decoded {:?} expected {:?}",
            divergence,
            divergence.map(|index| inflated.get(index.saturating_sub(4)..index + 4)),
            divergence.map(|index| raw.get(index.saturating_sub(4)..index + 4)),
        );
        let Some(checksum) = compressed.get(compressed.len().saturating_sub(4)..) else {
            return;
        };
        assert_eq!(checksum, &adler32(&raw).to_be_bytes()[..]);
    }

    #[test]
    fn the_compressor_actually_compresses() {
        let mut raw = Vec::new();
        for _ in 0..2000 {
            raw.extend_from_slice(&[10, 20, 30, 40]);
        }
        let compressed = zlib(&raw);
        assert!(
            compressed.len() < raw.len() / 4,
            "{} bytes for {} of repetition",
            compressed.len(),
            raw.len()
        );
    }

    #[test]
    fn an_image_encodes_as_a_png_that_decompresses_to_the_pixels() {
        let mut image = Image::new(8, 4);
        image.sky([0.0, 0.0, 0.2], [0.9, 0.6, 0.3]);
        let png = image.to_png();
        assert_eq!(
            png.get(0..8),
            Some(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a][..])
        );
        // IHDR follows, with the dimensions in it.
        assert_eq!(png.get(12..16), Some(&b"IHDR"[..]));
        let Some(width) = png.get(16..20) else {
            return;
        };
        assert_eq!(width, &8_u32.to_be_bytes()[..]);
        assert_eq!(png.get(20..24), Some(&4_u32.to_be_bytes()[..]));
        // The IEND chunk closes the file.
        assert_eq!(png.get(png.len() - 8..png.len() - 4), Some(&b"IEND"[..]));
        // And the IDAT payload decompresses to scanlines of the pixels that went in.
        let Some(start) = png.windows(4).position(|window| window == b"IDAT") else {
            return;
        };
        let Some(length) = png
            .get(start.saturating_sub(4)..start)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map(u32::from_be_bytes)
        else {
            return;
        };
        let length = length as usize;
        let Some(compressed) = png.get(start + 4..start + 4 + length) else {
            return;
        };
        let Some(deflate) = compressed.get(2..compressed.len().saturating_sub(4)) else {
            return;
        };
        let raw = inflate_fixed(deflate);
        assert_eq!(raw.len(), 4 * (1 + 8 * 3));
        assert_eq!(raw.first().copied(), Some(0));
        assert_eq!(raw.get(1..4), Some(&image.pixel(0, 0)[..]));
        assert_eq!(raw.get(raw.len() - 3..), Some(&image.pixel(7, 3)[..]));
    }

    #[test]
    fn colours_parse_from_hex_and_json() {
        assert_eq!(colour("#ff0000"), [1.0, 0.0, 0.0]);
        assert_eq!(colour("00ff00"), [0.0, 1.0, 0.0]);
        let value = jsonr::parse("[0.25, 0.5, 0.75]").or_die();
        assert_eq!(json_colour(Some(&value), [0.0; 3]), [0.25, 0.5, 0.75]);
        assert_eq!(json_colour(None, [1.0; 3]), [1.0; 3]);
    }

    #[test]
    fn a_triangle_covers_pixels_and_writes_depth() {
        let camera = Camera {
            eye: Vec3::new(0.0, 0.0, 5.0),
            target: Vec3::ZERO,
            fov_y: 1.0,
            near: 0.1,
            far: 100.0,
        };
        let mut image = Image::new(64, 64);
        image.sky([0.1, 0.1, 0.2], [0.1, 0.1, 0.2]);
        let renderer = Renderer::new(&camera, 1.0, Atmosphere::default());
        let face = Face {
            vertices: [
                Vertex {
                    world: Vec3::new(-1.0, -1.0, 0.0),
                    normal: Vec3::Z,
                },
                Vertex {
                    world: Vec3::new(1.0, -1.0, 0.0),
                    normal: Vec3::Z,
                },
                Vertex {
                    world: Vec3::new(0.0, 1.0, 0.0),
                    normal: Vec3::Z,
                },
            ],
            albedo: [0.9, 0.2, 0.1],
            style: Style::Lit,
        };
        renderer.triangle(&mut image, &face);
        assert!(image.coverage() > 0.05, "the triangle drew nothing");
        assert!(image.coverage() < 0.5, "the triangle covered too much");
        let centre = image.pixel(32, 40);
        assert!(centre[0] > centre[1], "the albedo did not reach the pixel");
    }

    #[test]
    fn a_triangle_behind_the_eye_draws_nothing() {
        let camera = Camera {
            eye: Vec3::ZERO,
            target: Vec3::new(0.0, 0.0, -1.0),
            fov_y: 1.0,
            near: 0.1,
            far: 100.0,
        };
        let mut image = Image::new(32, 32);
        let renderer = Renderer::new(&camera, 1.0, Atmosphere::default());
        let face = Face {
            vertices: [
                Vertex {
                    world: Vec3::new(-1.0, -1.0, 2.0),
                    normal: Vec3::Z,
                },
                Vertex {
                    world: Vec3::new(1.0, -1.0, 2.0),
                    normal: Vec3::Z,
                },
                Vertex {
                    world: Vec3::new(0.0, 1.0, 2.0),
                    normal: Vec3::Z,
                },
            ],
            albedo: [1.0; 3],
            style: Style::Lit,
        };
        renderer.triangle(&mut image, &face);
        assert_eq!(image.coverage(), 0.0);
    }

    #[test]
    fn a_nearer_triangle_wins_the_depth_test() {
        let camera = Camera {
            eye: Vec3::new(0.0, 0.0, 5.0),
            target: Vec3::ZERO,
            fov_y: 1.0,
            near: 0.1,
            far: 100.0,
        };
        let mut image = Image::new(32, 32);
        let renderer = Renderer::new(&camera, 1.0, Atmosphere::default());
        let quad = |z: f32, albedo: [f32; 3]| Face {
            vertices: [
                Vertex {
                    world: Vec3::new(-2.0, -2.0, z),
                    normal: Vec3::Z,
                },
                Vertex {
                    world: Vec3::new(2.0, -2.0, z),
                    normal: Vec3::Z,
                },
                Vertex {
                    world: Vec3::new(0.0, 2.0, z),
                    normal: Vec3::Z,
                },
            ],
            albedo,
            style: Style::Lit,
        };
        // The far one first, then the near one: the near one must overwrite it.
        renderer.triangle(&mut image, &quad(-2.0, [0.0, 0.0, 1.0]));
        renderer.triangle(&mut image, &quad(2.0, [1.0, 0.0, 0.0]));
        let centre = image.pixel(16, 16);
        assert!(
            centre[0] > centre[2],
            "the farther triangle won: {centre:?}"
        );
    }
}
