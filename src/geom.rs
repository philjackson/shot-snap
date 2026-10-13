/// Rectangle in global logical (compositor) coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn from_points(a: (f64, f64), b: (f64, f64)) -> Self {
        let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
        let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
        Self { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    pub fn contains(&self, p: (f64, f64)) -> bool {
        p.0 >= self.x && p.0 < self.right() && p.1 >= self.y && p.1 < self.bottom()
    }

    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        (x1 > x0 && y1 > y0).then_some(Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
    }

    pub fn union(&self, o: &Rect) -> Rect {
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        Rect { x: x0, y: y0, w: self.right().max(o.right()) - x0, h: self.bottom().max(o.bottom()) - y0 }
    }

    /// Snaps edges to whole logical pixels so crops never straddle pixels on integer scales.
    pub fn rounded(&self) -> Rect {
        let x0 = self.x.round();
        let y0 = self.y.round();
        Rect { x: x0, y: y0, w: self.right().round() - x0, h: self.bottom().round() - y0 }
    }
}
