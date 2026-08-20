//! Scroll arithmetic for a virtual canvas of stacked, individually focusable
//! blocks (the detail view's segments, the comments overlay's comment widgets).
//!
//! Pure because viewport arithmetic is where off-by-ones live, and because the
//! interesting cases — a block taller than the viewport, read-only rows above
//! the first block and below the last — are awkward to reach by hand in a
//! terminal.

/// Direction of travel for a single `j`/`k` step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
}

/// What a single `j`/`k` step should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Apply this absolute scroll offset; the focused block does not change.
    Scroll(usize),
    /// Nothing left to scroll here — advance focus, then call [`reveal_block`].
    MoveFocus,
    /// Already at the end of the canvas in this direction.
    Stop,
}

/// Scroll offset that reveals `block` = `(top, bottom)`, given the direction we
/// arrived from.
///
/// A block taller than the viewport can never be shown whole, so it aligns to
/// its *leading* edge — top when moving down, bottom when moving up — leaving
/// the following keypresses to scroll through the rest in the same direction
/// rather than bouncing back.
#[must_use]
pub const fn reveal_block(
    scroll: usize,
    viewport_h: usize,
    block: (usize, usize),
    dir: Dir,
) -> usize {
    let (top, bottom) = block;
    if viewport_h == 0 {
        return scroll;
    }
    if bottom.saturating_sub(top) > viewport_h {
        return match dir {
            Dir::Down => top,
            Dir::Up => bottom.saturating_sub(viewport_h),
        };
    }
    if bottom > scroll + viewport_h {
        bottom.saturating_sub(viewport_h)
    } else if top < scroll {
        top
    } else {
        scroll
    }
}

/// One `j`/`k` step: scroll through the focused block while part of it is off
/// screen, otherwise hand over to the next focusable block, otherwise scroll
/// into the read-only margins above the first / below the last block.
///
/// `block` is the focused block's `(top, bottom)`; `has_neighbour` says whether
/// another focusable block exists in `dir`; `lines` is the step size.
#[must_use]
pub fn step(
    dir: Dir,
    scroll: usize,
    viewport_h: usize,
    content_h: usize,
    block: (usize, usize),
    has_neighbour: bool,
    lines: usize,
) -> Step {
    if viewport_h == 0 {
        return Step::Stop;
    }
    let max_scroll = content_h.saturating_sub(viewport_h);
    let (top, bottom) = block;
    match dir {
        Dir::Down => {
            // Part of the focused block is below the fold — walk through it.
            if bottom > scroll + viewport_h {
                let target = (scroll + lines).min(bottom.saturating_sub(viewport_h));
                return Step::Scroll(target.min(max_scroll));
            }
            if has_neighbour {
                return Step::MoveFocus;
            }
            // Last block, but trailing read-only rows remain.
            if scroll < max_scroll {
                Step::Scroll((scroll + lines).min(max_scroll))
            } else {
                Step::Stop
            }
        }
        Dir::Up => {
            // Part of the focused block is above the fold — walk back through it.
            if scroll > top {
                return Step::Scroll(scroll.saturating_sub(lines).max(top));
            }
            if has_neighbour {
                return Step::MoveFocus;
            }
            // First block, but the header above it is still hidden.
            if scroll > 0 {
                Step::Scroll(scroll.saturating_sub(lines))
            } else {
                Step::Stop
            }
        }
    }
}

/// Borders for a bordered block inside a virtually-scrolled canvas.
///
/// A border row drawn on a clipped edge lands where a content row belongs and
/// hides it — most visibly the last line of an over-long description, which
/// would otherwise sit behind the bottom border and never be readable. So the
/// edge that is scrolled out of view loses its border instead. Titles go with
/// the top border: ratatui reserves the block's top row for a title even when
/// there is no top border to put it on.
#[must_use]
pub fn clipped_borders(clipped_top: bool, clipped_bottom: bool) -> ratatui::widgets::Borders {
    use ratatui::widgets::Borders;
    let mut borders = Borders::LEFT | Borders::RIGHT;
    if !clipped_top {
        borders |= Borders::TOP;
    }
    if !clipped_bottom {
        borders |= Borders::BOTTOM;
    }
    borders
}

#[cfg(test)]
mod tests {
    use super::{Dir, Step, reveal_block, step};

    const VIEWPORT: usize = 10;

    #[test]
    fn a_block_taller_than_the_viewport_scrolls_line_by_line_before_focus_moves() {
        // Header 0..4, description 4..30, another field below.
        let block = (4, 30);
        let mut scroll = 4;
        let mut steps = 0;
        loop {
            match step(Dir::Down, scroll, VIEWPORT, 40, block, true, 1) {
                Step::Scroll(n) => {
                    assert!(n > scroll, "must make progress");
                    scroll = n;
                    steps += 1;
                    assert!(steps < 100, "should terminate");
                }
                Step::MoveFocus => break,
                Step::Stop => panic!("should hand over to the next field, not stop"),
            }
        }
        // Walked to the description's last line, then handed over.
        assert_eq!(scroll, 20);
        assert_eq!(steps, 16);
    }

    #[test]
    fn the_first_block_scrolls_up_into_the_header() {
        // Comments widget at 4..7, nothing focusable above it.
        let block = (4, 7);
        assert_eq!(
            step(Dir::Up, 4, VIEWPORT, 40, block, false, 1),
            Step::Scroll(3)
        );
        assert_eq!(
            step(Dir::Up, 1, VIEWPORT, 40, block, false, 1),
            Step::Scroll(0)
        );
        // Already at the top of the canvas.
        assert_eq!(step(Dir::Up, 0, VIEWPORT, 40, block, false, 1), Step::Stop);
    }

    #[test]
    fn the_last_block_scrolls_down_into_the_trailing_rows_and_stops_at_the_end() {
        // Last field at 20..24, content runs to 40 → max scroll 30.
        let block = (20, 24);
        assert_eq!(
            step(Dir::Down, 25, VIEWPORT, 40, block, false, 1),
            Step::Scroll(26)
        );
        assert_eq!(
            step(Dir::Down, 30, VIEWPORT, 40, block, false, 1),
            Step::Stop
        );
    }

    #[test]
    fn no_step_ever_scrolls_past_the_end_of_the_content() {
        let max_scroll = 40 - VIEWPORT;
        for scroll in 0..=max_scroll {
            for &has_neighbour in &[true, false] {
                for &block in &[(0, 3), (4, 30), (36, 40)] {
                    if let Step::Scroll(n) =
                        step(Dir::Down, scroll, VIEWPORT, 40, block, has_neighbour, 7)
                    {
                        assert!(n <= max_scroll, "scroll {n} past max {max_scroll}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_short_block_below_the_fold_is_revealed_from_either_direction() {
        assert_eq!(reveal_block(0, VIEWPORT, (14, 17), Dir::Down), 7);
        assert_eq!(reveal_block(20, VIEWPORT, (14, 17), Dir::Up), 14);
        // Already fully visible — leave the offset alone.
        assert_eq!(reveal_block(10, VIEWPORT, (14, 17), Dir::Down), 10);
    }

    #[test]
    fn an_oversized_block_is_revealed_from_its_leading_edge() {
        // Entered from above: show its start. From below: show its end.
        assert_eq!(reveal_block(0, VIEWPORT, (4, 30), Dir::Down), 4);
        assert_eq!(reveal_block(35, VIEWPORT, (4, 30), Dir::Up), 20);
    }

    #[test]
    fn a_zero_height_viewport_is_inert() {
        assert_eq!(step(Dir::Down, 3, 0, 40, (0, 5), true, 1), Step::Stop);
        assert_eq!(reveal_block(3, 0, (0, 5), Dir::Down), 3);
    }
}
