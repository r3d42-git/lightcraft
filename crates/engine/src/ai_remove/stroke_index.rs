//! Ordered brush capsules in the frozen, transformed pixel coordinate system.
//! A small spatial index avoids testing a complete freehand history per pixel.
use super::Spot;

struct Capsule {
    a: [f64; 2],
    b: [f64; 2],
    radius: f64,
    subtract: bool,
}
pub(super) struct StrokeIndex {
    dims: [f64; 2],
    bins: Vec<Vec<usize>>,
    capsules: Vec<Capsule>,
}

impl StrokeIndex {
    const SIDE: usize = 32;
    fn coordinate(dims: [f64; 2], point: [f64; 2]) -> [usize; 2] {
        std::array::from_fn(|i| ((point[i] / dims[i] * Self::SIDE as f64).floor().max(0.) as usize).min(Self::SIDE - 1))
    }
    pub fn new(dims: [f64; 2], strokes: &[(&Spot, bool, bool)]) -> Result<Self, String> {
        let mut index =
            Self { dims, bins: if strokes.is_empty() { Vec::new() } else { vec![Vec::new(); Self::SIDE * Self::SIDE] }, capsules: Vec::new() };
        let mut work = 0usize;
        for &(spot, continuous, subtract) in strokes {
            if !spot.size.is_finite()
                || spot.size <= 1e-7
                || spot.points.len() > 4096
                || spot.points.iter().any(|p| !p.x.is_finite() || !p.y.is_finite())
            {
                return Err("Invalid brush mask.".into());
            }
            let radius = (spot.size - 1e-7) * dims[0].max(dims[1]);
            for (i, a) in spot.points.iter().enumerate() {
                let b = if continuous { spot.points.get(i + 1).unwrap_or(a) } else { a };
                let (a, b) = ([a.x * dims[0], a.y * dims[1]], [b.x * dims[0], b.y * dims[1]]);
                let lo = Self::coordinate(dims, [a[0].min(b[0]) - radius, a[1].min(b[1]) - radius]);
                let hi = Self::coordinate(dims, [a[0].max(b[0]) + radius, a[1].max(b[1]) + radius]);
                work = work.saturating_add((hi[0] + 1 - lo[0]) * (hi[1] + 1 - lo[1]));
                if work > 2_000_000 {
                    return Err("Selection protection is too complex. Try fewer correction strokes.".into());
                }
                for y in lo[1]..=hi[1] {
                    for x in lo[0]..=hi[0] {
                        index.bins[y * Self::SIDE + x].push(index.capsules.len());
                    }
                }
                index.capsules.push(Capsule { a, b, radius, subtract });
            }
        }
        Ok(index)
    }
    pub fn at(&self, point: [f64; 2]) -> Option<(bool, f32)> {
        if self.capsules.is_empty() {
            return None;
        }
        let [x, y] = Self::coordinate(self.dims, point);
        let mut alpha: f32 = 0.;
        for &i in self.bins[y * Self::SIDE + x].iter().rev() {
            let c = &self.capsules[i];
            let distance = lightcraft_develop::ai_edit::brush_distance(&[c.a, c.b], point, true);
            if distance >= c.radius {
                continue;
            }
            if c.subtract {
                return Some((alpha == 0., alpha));
            }
            let radius = c.radius + self.dims[0].max(self.dims[1]) * 1e-7;
            let t = ((radius - distance) / (0.15 * radius)).clamp(0., 1.);
            alpha = alpha.max((t * t * (3. - 2. * t)) as f32);
            if alpha == 1. {
                break;
            }
        }
        (alpha > 0.).then_some((false, alpha))
    }
}
