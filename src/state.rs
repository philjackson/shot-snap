//! Things shot-snap remembers between runs (not settings): `$XDG_STATE_HOME/shot-snap/state.toml`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::annotate::{ArrowHeads, Tool, HIGHLIGHT_COLORS};
use crate::config::{xdg_dir, Color};
use crate::geom::Rect;
use crate::paint::{default_gradients, Fill, Gradient, DEFAULT_SOLIDS};
use crate::style::Style;

pub const RECENT_LIMIT: usize = 10;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    last_region: Option<[f64; 4]>,
    pub annotate: AnnotateState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnnotateState {
    /// What new shapes are painted with.
    pub fill: Fill,
    /// Most recent first. The built-in defaults are never listed here.
    pub recent_solids: Vec<Color>,
    pub recent_gradients: Vec<Gradient>,
    /// Stroke width per tool, in logical pixels.
    pub tool_widths: BTreeMap<String, f64>,
    /// The fine size stepper is showing.
    pub advanced_size: bool,
    /// Selected tool, and which ends arrows get heads on.
    pub tool: Tool,
    pub arrow_heads: ArrowHeads,
    /// The highlighter's own colour, separate from the colour panel.
    pub highlight_color: Color,
    /// Screenshot style (padded backdrop, rounded corners, shadow).
    pub style: Style,
}

impl Default for AnnotateState {
    fn default() -> Self {
        Self {
            fill: Fill::default(),
            recent_solids: Vec::new(),
            recent_gradients: Vec::new(),
            tool_widths: Default::default(),
            advanced_size: false,
            tool: Tool::default(),
            arrow_heads: ArrowHeads::default(),
            highlight_color: HIGHLIGHT_COLORS[0].1,
            style: Style::default(),
        }
    }
}

impl AnnotateState {
    /// Tools without a size control always use their default.
    pub fn width(&self, tool: Tool) -> f64 {
        match tool.has_size() {
            true => self.tool_widths.get(tool.name()).copied().unwrap_or(tool.default_width()),
            false => tool.default_width(),
        }
    }

    pub fn set_width(&mut self, tool: Tool, width: f64) {
        if !tool.has_size() {
            return;
        }
        let (min, max) = tool.width_range();
        self.tool_widths.insert(tool.name().into(), width.clamp(min, max));
    }

    /// Records the current fill as a recent pick, unless it's one of the defaults.
    pub fn remember_fill(&mut self) {
        match &self.fill {
            Fill::Solid(c) if !DEFAULT_SOLIDS.contains(c) => push_recent(&mut self.recent_solids, *c),
            Fill::Gradient(g) => {
                let g = g.normalized();
                if !default_gradients().contains(&g) {
                    push_recent(&mut self.recent_gradients, g);
                }
            }
            Fill::Solid(_) => {}
        }
    }
}

fn push_recent<T: PartialEq>(list: &mut Vec<T>, item: T) {
    list.retain(|x| *x != item);
    list.insert(0, item);
    list.truncate(RECENT_LIMIT);
}

fn path() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state").join("shot-snap/state.toml")
}

/// A missing or unreadable state file just means starting fresh.
pub fn load() -> State {
    let mut state: State = std::fs::read_to_string(path()).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default();
    let a = &mut state.annotate;
    a.recent_solids.truncate(RECENT_LIMIT);
    // Widths for tools without a size control (e.g. from older versions) mean nothing.
    a.tool_widths.retain(|name, _| Tool::ALL.iter().any(|t| t.has_size() && t.name() == name));
    a.recent_gradients.retain(|g| g.stops.len() >= 2);
    a.recent_gradients.truncate(RECENT_LIMIT);
    if matches!(&a.fill, Fill::Gradient(g) if g.stops.len() < 2) {
        a.fill = Fill::default();
    }
    state
}

pub fn save(state: &State) {
    let path = path();
    let result = toml::to_string(state)
        .map_err(std::io::Error::other)
        .and_then(|text| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, text)
        });
    if let Err(e) = result {
        log::warn!("could not save state to {}: {e}", path.display());
    }
}

pub fn last_region() -> Option<Rect> {
    load().last_region.map(|[x, y, w, h]| Rect { x, y, w, h })
}

pub fn save_last_region(rect: Rect) {
    let mut state = load();
    state.last_region = Some([rect.x, rect.y, rect.w, rect.h]);
    save(&state);
}

pub fn save_annotate(annotate: AnnotateState) {
    let mut state = load();
    state.annotate = annotate;
    save(&state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recents_dedupe_cap_and_skip_defaults() {
        let mut a = AnnotateState { fill: Fill::Solid(DEFAULT_SOLIDS[1]), ..Default::default() };
        a.remember_fill();
        assert!(a.recent_solids.is_empty());
        for i in 0..12 {
            a.fill = Fill::Solid(Color { r: i as f32 / 20.0, g: 0.0, b: 0.0, a: 1.0 });
            a.remember_fill();
        }
        assert_eq!(a.recent_solids.len(), RECENT_LIMIT);
        let again = a.recent_solids[5];
        a.fill = Fill::Solid(again);
        a.remember_fill();
        assert_eq!(a.recent_solids[0], again);
        assert_eq!(a.recent_solids.iter().filter(|c| **c == again).count(), 1);
    }

    #[test]
    fn state_round_trips_through_toml() {
        let mut state = State { last_region: Some([1.0, 2.0, 3.0, 4.0]), ..Default::default() };
        state.annotate.fill = Fill::Gradient(default_gradients()[2].clone());
        state.annotate.recent_solids = vec![DEFAULT_SOLIDS[3]];
        state.annotate.recent_gradients = vec![default_gradients()[0].clone()];
        let text = toml::to_string(&state).unwrap();
        let back: State = toml::from_str(&text).unwrap();
        assert_eq!(back.last_region, state.last_region);
        assert_eq!(back.annotate.recent_solids.len(), 1);
        assert_eq!(back.annotate.recent_gradients, state.annotate.recent_gradients);
        assert!(matches!(back.annotate.fill, Fill::Gradient(_)));
    }

    #[test]
    fn tool_widths_default_and_clamp() {
        let mut a = AnnotateState::default();
        assert_eq!(a.width(Tool::Rectangle), 6.0);
        assert_eq!(a.width(Tool::Text), 24.0);
        a.set_width(Tool::Rectangle, 500.0);
        assert_eq!(a.width(Tool::Rectangle), 100.0);
        a.set_width(Tool::Rectangle, 0.0);
        assert_eq!(a.width(Tool::Rectangle), 1.0);
        a.set_width(Tool::Text, 2.0);
        assert_eq!(a.width(Tool::Text), 8.0);
        a.tool = Tool::Arrow;
        a.arrow_heads = ArrowHeads::Both;
        let back: State = toml::from_str(&toml::to_string(&State { annotate: a, ..Default::default() }).unwrap()).unwrap();
        assert_eq!(back.annotate.width(Tool::Rectangle), 1.0);
        assert_eq!((back.annotate.tool, back.annotate.arrow_heads), (Tool::Arrow, ArrowHeads::Both));

        // Highlight colour defaults to yellow and round-trips.
        let mut h = AnnotateState::default();
        assert_eq!(h.highlight_color, HIGHLIGHT_COLORS[0].1);
        h.highlight_color = HIGHLIGHT_COLORS[1].1;
        let back: State = toml::from_str(&toml::to_string(&State { annotate: h, ..Default::default() }).unwrap()).unwrap();
        assert_eq!(back.annotate.highlight_color, HIGHLIGHT_COLORS[1].1);

        // Blur has no size control: it stays at 32 whatever is asked or stored.
        let mut a = AnnotateState::default();
        a.set_width(Tool::Blur, 10.0);
        a.tool_widths.insert("blur".into(), 80.0);
        assert_eq!(a.width(Tool::Blur), 32.0);
    }
}
