//! Small drawing primitives in the style of btop: filled braille graphs with a color gradient,
//! bar charts with eighth-block precision and segmented meters.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;

use super::theme::{self, Gradient, Tone};

/// One graph column: stacked segments from the bottom, as (cumulative top value, color).
/// A single segment with `None` color uses the graph's vertical gradient.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Column {
    pub segments: Vec<(f64, Option<Tone>)>,
}

impl Column {
    pub fn single(v: f64) -> Self {
        Self {
            segments: vec![(v, None)],
        }
    }

    pub fn top(&self) -> f64 {
        self.segments.last().map_or(0.0, |s| s.0)
    }

    fn color_at(&self, v: f64) -> Option<Tone> {
        self.segments
            .iter()
            .find(|s| s.0 >= v)
            .or(self.segments.last())
            .and_then(|s| s.1)
    }
}

/// A filled area graph drawn with braille dots, two columns per cell.
/// `columns` holds one entry per dot column (2 × width); `None` means no data.
pub fn area(
    buf: &mut Buffer,
    area: Rect,
    columns: &[Option<Column>],
    max: f64,
    gradient: Gradient,
) {
    if area.width == 0 || area.height == 0 || max <= 0.0 {
        return;
    }
    let dots_h = area.height as f64 * 4.0;
    let filled: Vec<Option<(usize, &Column)>> = columns
        .iter()
        .map(|c| {
            c.as_ref().map(|c| {
                (
                    ((c.top() / max).clamp(0.0, 1.0) * dots_h).round() as usize,
                    c,
                )
            })
        })
        .collect();
    for cy in 0..area.height {
        // Dot rows covered by this cell, counted from the bottom.
        let bottom = (area.height - 1 - cy) as usize * 4;
        let y_frac = (bottom as f64 + 2.0) / dots_h;
        for cx in 0..area.width {
            let mut bits: u32 = 0;
            let mut color: Option<Tone> = None;
            for side in 0..2 {
                let Some(Some((n, col))) = filled.get(cx as usize * 2 + side) else {
                    continue;
                };
                for r in 0..4 {
                    // r = 0 is the top dot row of the cell.
                    if bottom + (3 - r) < *n {
                        bits |= braille_bit(side, r);
                    }
                }
                if *n > bottom && color.is_none() {
                    let in_cell = (*n - bottom).min(4) as f64;
                    let v = (bottom as f64 + in_cell / 2.0) / dots_h * max;
                    color = col.color_at(v);
                }
            }
            if bits == 0 {
                continue;
            }
            let c = color.unwrap_or_else(|| gradient.at(y_frac));
            let ch = char::from_u32(0x2800 + bits).unwrap_or(' ');
            if let Some(cell) = buf.cell_mut((area.x + cx, area.y + cy)) {
                cell.set_char(ch).set_fg(c.color());
            }
        }
    }
}

fn braille_bit(side: usize, row: usize) -> u32 {
    const LEFT: [u32; 4] = [0x01, 0x02, 0x04, 0x40];
    const RIGHT: [u32; 4] = [0x08, 0x10, 0x20, 0x80];
    if side == 0 { LEFT[row] } else { RIGHT[row] }
}

/// Resamples `points` (x, y) with x in [x0, x1] into `n` columns by averaging.
pub fn resample(points: &[(f64, f64)], x0: f64, x1: f64, n: usize) -> Vec<Option<f64>> {
    let mut sum = vec![0.0; n];
    let mut count = vec![0u32; n];
    let span = (x1 - x0).max(f64::MIN_POSITIVE);
    for (x, y) in points {
        let i = (((x - x0) / span) * n as f64).floor();
        if i >= 0.0 && (i as usize) < n {
            sum[i as usize] += y;
            count[i as usize] += 1;
        }
    }
    (0..n)
        .map(|i| (count[i] > 0).then(|| sum[i] / count[i] as f64))
        .collect()
}

/// Draws a dashed horizontal marker line at `value` where the graph leaves room.
pub fn hline(buf: &mut Buffer, area: Rect, value: f64, max: f64, color: Tone) {
    if max <= 0.0 || area.height == 0 || value <= 0.0 {
        return;
    }
    let frac = (value / max).clamp(0.0, 1.0);
    let row = area.height - 1 - ((frac * area.height as f64).floor() as u16).min(area.height - 1);
    for x in 0..area.width {
        if let Some(cell) = buf.cell_mut((area.x + x, area.y + row))
            && cell.symbol() == " "
            && x % 2 == 0
        {
            cell.set_char('╌').set_fg(color.color());
        }
    }
}

/// Vertical bars drawn with braille dots, like btop's graphs. Each bar is colored with its
/// entry in `colors`; without colors the bars follow the vertical `gradient`.
pub struct Bars<'a> {
    pub values: &'a [f64],
    pub colors: &'a [Tone],
    pub max: f64,
    pub width: u16,
    pub gap: u16,
    pub highlight: Option<usize>,
    pub gradient: Gradient,
}

impl Bars<'_> {
    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        if self.max <= 0.0 || area.width == 0 {
            return;
        }
        let mut columns: Vec<Option<Column>> = vec![None; area.width as usize * 2];
        for (i, v) in self.values.iter().enumerate() {
            let x0 = i as u16 * (self.width + self.gap);
            if x0 + self.width > area.width {
                break;
            }
            let color = self.colors.get(i).copied().map(|c| {
                if self.highlight.is_some() && self.highlight != Some(i) {
                    c.lerp(theme::t().faint, 0.3)
                } else {
                    c
                }
            });
            // Leave one dot column free between touching bars.
            let dots = (self.width * 2).saturating_sub(if self.gap == 0 && self.width > 0 {
                1
            } else {
                0
            });
            for d in 0..dots {
                let idx = (x0 * 2 + d) as usize;
                if let Some(slot) = columns.get_mut(idx) {
                    *slot = Some(Column {
                        segments: vec![(*v, color)],
                    });
                }
            }
        }
        self::area(buf, area, &columns, self.max, self.gradient);
    }
}

/// A one-row braille graph, two samples per cell, filled from the bottom.
pub fn dots(values: &[f64], width: usize, max: f64, color: Tone) -> Vec<Span<'static>> {
    let n = width * 2;
    // Fewer samples than dot columns: one per column, right aligned.
    let cols: Vec<Option<f64>> = if values.len() < n {
        std::iter::repeat_n(None, n - values.len())
            .chain(values.iter().map(|v| Some(*v)))
            .collect()
    } else {
        let pts: Vec<(f64, f64)> = values
            .iter()
            .enumerate()
            .map(|(i, v)| (i as f64, *v))
            .collect();
        resample(&pts, 0.0, values.len() as f64, n)
    };
    let level = |v: Option<f64>| -> usize {
        match v {
            Some(v) if max > 0.0 && v > 0.0 => {
                ((v / max).clamp(0.0, 1.0) * 4.0).round().max(1.0) as usize
            }
            _ => 0,
        }
    };
    (0..width)
        .map(|cx| {
            let mut bits = 0u32;
            for side in 0..2 {
                let l = level(cols[cx * 2 + side]);
                for r in 0..4 {
                    if 3 - r < l {
                        bits |= braille_bit(side, r);
                    }
                }
            }
            let ch = if bits == 0 {
                ' '
            } else {
                char::from_u32(0x2800 + bits).unwrap_or(' ')
            };
            Span::styled(ch.to_string(), Style::new().fg(color.color()))
        })
        .collect()
}

/// A segmented meter like btop's: `width` squares, filled ones colored along the gradient.
pub fn meter(frac: f64, width: usize, gradient: Gradient) -> Vec<Span<'static>> {
    let filled = (frac.clamp(0.0, 1.0) * width as f64).round() as usize;
    let mut spans = Vec::with_capacity(width);
    for i in 0..width {
        let t = if width > 1 {
            i as f64 / (width - 1) as f64
        } else {
            1.0
        };
        let style = if i < filled {
            Style::new().fg(gradient.color(t))
        } else {
            Style::new().fg(theme::t().faint.color())
        };
        spans.push(Span::styled("■", style));
    }
    spans
}

/// A meter in a single color.
pub fn meter_tone(frac: f64, width: usize, color: Tone) -> Vec<Span<'static>> {
    let filled = (frac.clamp(0.0, 1.0) * width as f64).round() as usize;
    vec![
        Span::styled("■".repeat(filled), Style::new().fg(color.color())),
        Span::styled(
            "■".repeat(width - filled.min(width)),
            Style::new().fg(theme::t().faint.color()),
        ),
    ]
}

/// Paints a text in a horizontal gradient.
pub fn gradient_text(text: &str, gradient: Gradient, style: Style) -> Vec<Span<'static>> {
    let n = text.chars().count().max(2) - 1;
    text.chars()
        .enumerate()
        .map(|(i, c)| Span::styled(c.to_string(), style.fg(gradient.color(i as f64 / n as f64))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_fills_from_the_bottom() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 2));
        let cols = vec![Some(Column::single(50.0)), Some(Column::single(100.0))];
        let r = buf.area;
        area(&mut buf, r, &cols, 100.0, theme::t().power);
        // Bottom cell: both columns full. Top cell: only the right column.
        assert_eq!(buf[(0, 1)].symbol(), "⣿");
        assert_eq!(buf[(0, 0)].symbol(), "⢸");
    }

    #[test]
    fn resample_averages_into_columns() {
        let pts = [(0.0, 1.0), (0.4, 3.0), (0.6, 10.0)];
        assert_eq!(resample(&pts, 0.0, 1.0, 2), vec![Some(2.0), Some(10.0)]);
        assert_eq!(resample(&[], 0.0, 1.0, 2), vec![None, None]);
    }

    #[test]
    fn bars_are_dotted_with_a_gap_between_bars() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
        Bars {
            values: &[1.0, 0.5],
            colors: &[],
            max: 1.0,
            width: 1,
            gap: 0,
            highlight: None,
            gradient: theme::t().power,
        }
        .render(&mut buf, Rect::new(0, 0, 2, 1));
        assert_eq!(buf[(0, 0)].symbol(), "⡇");
        assert_eq!(buf[(1, 0)].symbol(), "⡄");
    }

    #[test]
    fn dots_fill_from_the_bottom() {
        let d = dots(&[4.0, 2.0, 0.0, 1.0], 2, 4.0, theme::t().cpu);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].content, "⣧");
        assert_eq!(d[1].content, "⢀");
    }

    #[test]
    fn meter_fills_proportionally() {
        let m = meter(0.5, 4, theme::t().power);
        assert_eq!(m.len(), 4);
        assert_ne!(m[1].style, m[2].style);
    }
}
