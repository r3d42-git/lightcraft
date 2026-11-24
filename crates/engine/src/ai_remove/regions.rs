//! Coarse transformed-space cells only choose inference windows. Authorization is
//! always sampled again from the actual mask on the source grid.
use super::super::{SelectionMask, Spot};
use std::collections::VecDeque;

pub(super) struct Plan {
    size: [usize; 2],
    step: [f64; 2],
    owners: Vec<u8>,
    pub windows: Vec<Window>,
}
pub(super) struct Window {
    pub bounds: [f64; 4],
    pub cable: bool,
    pub group: usize,
}

impl Plan {
    pub fn owner(&self, point: [f64; 2]) -> usize {
        let [x, y] = std::array::from_fn(|i| ((point[i] / self.step[i]).floor().max(0.) as usize).min(self.size[i] - 1));
        self.owners[y * self.size[0] + x] as usize
    }
}

pub(super) fn plan(dimensions: [f64; 2], segmented: Option<&SelectionMask>, strokes: &[(&Spot, bool)], slots: usize) -> Option<Plan> {
    if slots < 2 || dimensions.iter().copied().fold(0.0f64, f64::max) <= 512. {
        return None;
    }
    let cell = (dimensions[0].max(dimensions[1]) / 256.).max(8.);
    let size = dimensions.map(|v| (v / cell).ceil().max(1.) as usize);
    let step = std::array::from_fn(|i| dimensions[i] / size[i] as f64);
    let mut grid = vec![false; size[0] * size[1]];
    let range = |low: f64, high: f64, axis: usize| {
        ((low / step[axis]).floor().max(0.) as usize).min(size[axis])..((high / step[axis]).ceil().max(0.) as usize).min(size[axis])
    };
    if let Some(mask) = segmented {
        let (side, rect, logits) = mask.grid();
        let dx = (rect[2] - rect[0]) * dimensions[0] / side as f64;
        let dy = (rect[3] - rect[1]) * dimensions[1] / side as f64;
        for (i, _) in logits.iter().enumerate().filter(|(_, value)| **value > 0.) {
            let x = rect[0] * dimensions[0] + (i % side) as f64 * dx;
            let y = rect[1] * dimensions[1] + (i / side) as f64 * dy;
            for row in range(y, y + dy, 1) {
                for col in range(x, x + dx, 0) {
                    grid[row * size[0] + col] = true;
                }
            }
        }
    }
    let mut work = 0usize;
    for &(spot, continuous) in strokes {
        let radius = spot.size * dimensions[0].max(dimensions[1]);
        let conservative = radius + step[0].hypot(step[1]) * 0.5;
        for (i, a) in spot.points.iter().enumerate() {
            let b = if continuous { spot.points.get(i + 1).unwrap_or(a) } else { a };
            let (a, b) = ([a.x * dimensions[0], a.y * dimensions[1]], [b.x * dimensions[0], b.y * dimensions[1]]);
            let cols = range(a[0].min(b[0]) - radius, a[0].max(b[0]) + radius, 0);
            let rows = range(a[1].min(b[1]) - radius, a[1].max(b[1]) + radius, 1);
            work = work.saturating_add(cols.len().saturating_mul(rows.len()));
            if work > 8_000_000 {
                return None;
            }
            for row in rows {
                for col in cols.clone() {
                    let p = [(col as f64 + 0.5) * step[0], (row as f64 + 0.5) * step[1]];
                    if lightcraft_develop::ai_edit::brush_distance(&[a, b], p, true) <= conservative {
                        grid[row * size[0] + col] = true;
                    }
                }
            }
        }
    }
    let neighbours = |i: usize| {
        let (x, y) = (i % size[0], i / size[0]);
        (y.saturating_sub(1)..=(y + 1).min(size[1] - 1))
            .flat_map(move |yy| (x.saturating_sub(1)..=(x + 1).min(size[0] - 1)).map(move |xx| yy * size[0] + xx))
    };
    let bounds = |cells: &[usize]| {
        let mut b = [size[0], size[1], 0, 0];
        for &i in cells {
            let (x, y) = (i % size[0], i / size[0]);
            b = [b[0].min(x), b[1].min(y), b[2].max(x + 1), b[3].max(y + 1)];
        }
        [b[0] as f64 * step[0], b[1] as f64 * step[1], b[2] as f64 * step[0], b[3] as f64 * step[1]]
    };
    let mut components = Vec::new();
    for i in 0..grid.len() {
        if !std::mem::take(&mut grid[i]) {
            continue;
        }
        let mut cells = vec![i];
        let mut next = 0;
        while let Some(&cell) = cells.get(next) {
            next += 1;
            for j in neighbours(cell) {
                if std::mem::take(&mut grid[j]) {
                    cells.push(j);
                }
            }
        }
        if components.len() >= 128 {
            return None;
        } // Fragmented noise stays one conventional selection.
        components.push(cells);
    }
    // An accessory next to a person needs the same context as the person.
    // Merge when each support reaches into the other's model window.
    let context = |b| super::context_rect(b, dimensions[0] as usize, dimensions[1] as usize);
    let intersects = |a: [f64; 4], b: [f64; 4]| a[0] < b[2] && a[2] > b[0] && a[1] < b[3] && a[3] > b[1];
    let mut i = 0;
    while i < components.len() {
        let b = bounds(&components[i]);
        if let Some(c) = context(b)
            && let Some(j) = (i + 1..components.len()).find(|&j| {
                let other = bounds(&components[j]);
                let dx = (b[0] - other[2]).max(other[0] - b[2]).max(0.);
                let dy = (b[1] - other[3]).max(other[1] - b[3]).max(0.);
                dx.hypot(dy) <= 192. && intersects(other, c) && context(other).is_some_and(|r| intersects(b, r))
            })
        {
            let other = components.remove(j);
            components[i].extend(other);
            i = 0;
        } else {
            i += 1;
        }
    }
    let groups = components.len();
    if groups > slots {
        return None;
    }
    let mut chunks = Vec::new();
    for (index, cells) in components.into_iter().enumerate() {
        let b = bounds(&cells);
        let span = (b[2] - b[0]).max(b[3] - b[1]);
        // Mean occupied cross-section also detects diagonal and bent wires
        // whose bounding boxes can be almost square.
        let thickness = cells.len() as f64 * step[0] * step[1] / span;
        let minor = (b[2] - b[0]).min(b[3] - b[1]);
        // Several nearby wires or a slender pole increase absolute thickness.
        // Their occupied cross-section stays small relative to their length.
        let elongated_bundle = span >= minor * 5. && thickness <= span * 0.12;
        let cable = span > 850. && (thickness <= (span * 0.14).min(150.) || elongated_bundle);
        let available = slots - chunks.len() - (groups - index - 1);
        let pieces = if cable { ((span / 760.).ceil() as usize).min(available) } else { 1 };
        if chunks.len() + pieces > slots {
            return None;
        }
        let axis = usize::from(b[3] - b[1] > b[2] - b[0]);
        let mut bins = vec![Vec::new(); pieces];
        for cell in cells {
            let xy = [cell % size[0], cell / size[0]];
            let distance = (xy[axis] as f64 + 0.5) * step[axis] - b[axis];
            bins[((distance / span * pieces as f64) as usize).min(pieces - 1)].push(cell);
        }
        for bin in bins.into_iter().filter(|b| !b.is_empty()) {
            chunks.push((bin, cable, index));
        }
    }
    if chunks.len() < 2 {
        return None;
    }
    let mut owners = vec![u8::MAX; grid.len()];
    let mut queue = VecDeque::new();
    let mut windows = Vec::new();
    for (i, (cells, cable, group)) in chunks.into_iter().enumerate() {
        windows.push(Window { bounds: bounds(&cells), cable, group });
        for cell in cells {
            owners[cell] = i as u8;
            queue.push_back(cell);
        }
    }
    // Assign context as well as selection cells to a unique tile. Expanded
    // effects masks can then never claim the same source pixel twice.
    while let Some(cell) = queue.pop_front() {
        for j in neighbours(cell) {
            if owners[j] == u8::MAX {
                owners[j] = owners[cell];
                queue.push_back(j);
            }
        }
    }
    Some(Plan { size, step, owners, windows })
}
