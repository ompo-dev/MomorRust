pub(crate) const NOTES: usize = 0;
pub(crate) const BROWSER: usize = 1;
pub(crate) const CHAT: usize = 2;
const MIN_WIDTHS: [f32; 3] = [280., 50., 300.];

#[derive(Clone, Copy, Debug)]
pub(crate) struct PanelResize {
    pub width: f32,
    pub origin_offset: f32,
    pub previous: [bool; 3],
    pub previous_offsets: [f32; 3],
}

#[derive(Clone, Copy)]
pub(crate) struct PanelAnimation {
    pub from_origin: f32,
    pub from_width: f32,
    pub to_origin: f32,
    pub to_width: f32,
    pub widths: [f32; 3],
    pub from_widths: [f32; 3],
    pub visible: [bool; 3],
}

pub(crate) struct AnimationFrame {
    pub origin: f32,
    pub width: f32,
    pub visible: [bool; 3],
    pub widths: [f32; 3],
    pub positions: [f32; 3],
    pub offsets: [f32; 3],
}

impl PanelAnimation {
    pub fn frame(&self, progress: f32, current_origin: f32) -> AnimationFrame {
        let finished = progress >= 1.;
        let progress = 1. - (1. - progress.clamp(0., 1.)).powi(3);
        let interpolate = |from: f32, to: f32| from + (to - from) * progress;
        let mut frame = AnimationFrame {
            origin: interpolate(self.from_origin, self.to_origin),
            width: interpolate(self.from_width, self.to_width),
            visible: [false; 3],
            widths: [0.; 3],
            positions: [0.; 3],
            offsets: [0.; 3],
        };
        let mut rendered_offset = 0.;
        for index in 0..3 {
            frame.visible[index] =
                self.visible[index] || (!finished && self.from_widths[index] > 0.);
            if !frame.visible[index] {
                continue;
            }
            let target_width = if self.visible[index] {
                self.widths[index]
            } else {
                0.
            };
            frame.widths[index] = interpolate(self.from_widths[index], target_width);
            frame.positions[index] = frame.origin + rendered_offset;
            frame.offsets[index] = frame.positions[index] - current_origin - rendered_offset;
            rendered_offset += frame.widths[index];
        }
        frame
    }
}

pub(crate) struct PanelLayout {
    visible: Option<[bool; 3]>,
    widths: [f32; 3],
    viewport_width: f32,
    overhead: f32,
    pending_width: Option<f32>,
    pending_origin: Option<f32>,
    window_bounds: Option<(f32, f32)>,
}

impl Default for PanelLayout {
    fn default() -> Self {
        Self {
            visible: None,
            widths: [420., 720., 400.],
            viewport_width: 0.,
            overhead: 0.,
            pending_width: None,
            pending_origin: None,
            window_bounds: None,
        }
    }
}

impl PanelLayout {
    pub fn width(&self, panel: usize) -> f32 {
        self.widths[panel]
    }

    pub fn resizing(&self) -> bool {
        self.pending_width.is_some()
    }

    pub fn sync_window_resize(
        &mut self,
        visible: [bool; 3],
        origin: f32,
        width: f32,
    ) -> Option<[f32; 3]> {
        let (previous_origin, previous_width) = self.window_bounds.replace((origin, width))?;
        if self.resizing() || self.visible != Some(visible) || (width - previous_width).abs() < 0.5
        {
            return None;
        }
        let panels = (0..3).filter(|&index| visible[index]).collect::<Vec<_>>();
        if panels.is_empty() {
            return None;
        }
        // A left-edge resize must not consume the chat's width on the right.
        self.resize_edge(&panels, previous_origin - origin);
        self.resize_edge(
            &panels.iter().rev().copied().collect::<Vec<_>>(),
            origin + width - previous_origin - previous_width,
        );
        Some(self.widths)
    }

    fn resize_edge(&mut self, panels: &[usize], mut delta: f32) {
        for &index in panels {
            let previous = self.widths[index];
            self.widths[index] = (previous + delta).max(MIN_WIDTHS[index].min(previous));
            delta -= self.widths[index] - previous;
            if delta.abs() < 0.5 {
                return;
            }
        }
        // The native window can be narrower than the sum of the panel minima.
        for index in [CHAT, NOTES, BROWSER] {
            if !panels.contains(&index) {
                continue;
            }
            let previous = self.widths[index];
            let minimum = if index == BROWSER {
                MIN_WIDTHS[BROWSER]
            } else {
                0.
            };
            let minimum = minimum.min(previous);
            self.widths[index] = (previous + delta).max(minimum);
            delta -= self.widths[index] - previous;
            if delta.abs() < 0.5 {
                break;
            }
        }
    }

    pub fn resize_divider(&mut self, panel: usize, width: f32) -> Option<[f32; 3]> {
        let visible = self.visible?;
        if self.resizing() || !visible[panel] {
            return None;
        }
        let neighbor = (panel + 1..3).find(|&index| visible[index])?;
        let available = self.widths[panel] + self.widths[neighbor];
        let minimum = MIN_WIDTHS[panel].min(available);
        let maximum = (available - MIN_WIDTHS[neighbor]).max(minimum);
        self.widths[panel] = width.clamp(minimum, maximum);
        self.widths[neighbor] = available - self.widths[panel];
        Some(self.widths)
    }

    pub fn update(
        &mut self,
        visible: [bool; 3],
        requested: [f32; 3],
        viewport_width: f32,
    ) -> Option<PanelResize> {
        let Some(previous) = self.visible else {
            self.widths = requested;
            self.visible = Some(visible);
            self.sample(visible, requested, viewport_width);
            return None;
        };
        if previous == visible {
            if let Some(expected) = self.pending_width {
                if (viewport_width - expected).abs() > 1. {
                    return None;
                }
                self.pending_width = None;
                self.pending_origin = None;
            }
            self.sample(visible, requested, viewport_width);
            return None;
        }
        if self.pending_width.is_none() && (viewport_width - self.viewport_width).abs() > 1. {
            self.sample(previous, requested, viewport_width);
        }
        let offset = |panels: [bool; 3], end: usize| {
            (0..end)
                .filter(|&index| panels[index])
                .map(|index| self.widths[index])
                .sum::<f32>()
        };
        let origin_offset = (0..3)
            .find(|&index| previous[index] && visible[index])
            .map(|anchor| offset(previous, anchor) - offset(visible, anchor))
            .unwrap_or_default();
        let width = if visible.iter().any(|open| *open) {
            ((0..3)
                .filter(|&index| visible[index])
                .map(|index| self.widths[index])
                .sum::<f32>()
                + self.overhead)
                .max(150.)
        } else {
            400.
        };
        self.visible = Some(visible);
        self.pending_width = Some(width);
        Some(PanelResize {
            width,
            origin_offset,
            previous,
            previous_offsets: std::array::from_fn(|index| offset(previous, index)),
        })
    }

    pub fn cancel_resize(&mut self) {
        self.pending_width = None;
        self.pending_origin = None;
    }

    pub fn resize_origin(
        &mut self,
        current_origin: f32,
        offset: f32,
        available_range: Option<(f32, f32)>,
    ) -> f32 {
        let origin = self.pending_origin.unwrap_or(current_origin) + offset;
        self.set_resize_origin(origin, available_range)
    }

    pub fn set_resize_origin(&mut self, origin: f32, available_range: Option<(f32, f32)>) -> f32 {
        let origin = available_range
            .map(|(minimum, maximum)| origin.clamp(minimum, maximum))
            .unwrap_or(origin);
        self.pending_origin = Some(origin);
        origin
    }

    fn sample(&mut self, visible: [bool; 3], requested: [f32; 3], viewport_width: f32) {
        let available = (viewport_width - self.overhead).max(0.);
        if visible[NOTES] {
            self.widths[NOTES] = if visible[BROWSER] || visible[CHAT] {
                requested[NOTES]
            } else {
                available
            };
        }
        let notes = if visible[NOTES] {
            self.widths[NOTES]
        } else {
            0.
        };
        if visible[BROWSER] {
            self.widths[BROWSER] = if visible[CHAT] {
                requested[BROWSER]
            } else {
                (available - notes).max(0.)
            };
        }
        if visible[CHAT] {
            let browser = if visible[BROWSER] {
                self.widths[BROWSER]
            } else {
                0.
            };
            self.widths[CHAT] = (available - notes - browser).max(0.);
        }
        self.viewport_width = viewport_width;
    }

    pub fn record_rendered_widths(
        &mut self,
        visible: [bool; 3],
        widths: &[f32],
        viewport_width: f32,
    ) {
        // Windows applies native bounds asynchronously. Do not measure the old
        // viewport into the remembered widths while a toggle resize is pending.
        if self.resizing() || self.visible != Some(visible) || !visible.iter().any(|open| *open) {
            return;
        }
        let mut widths = widths.iter().copied();
        for (index, open) in visible.into_iter().enumerate() {
            if open && let Some(width) = widths.next() {
                self.widths[index] = width;
            }
        }
        let occupied: f32 = (0..3)
            .filter(|&index| visible[index])
            .map(|index| self.widths[index])
            .sum();
        self.overhead = (viewport_width - occupied).max(0.);
        self.viewport_width = viewport_width;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded(visible: [bool; 3]) -> PanelLayout {
        let mut layout = PanelLayout::default();
        let widths = [420., 930., 386.];
        let width = (0..3)
            .filter(|&index| visible[index])
            .map(|index| widths[index])
            .sum();
        layout.update(visible, widths, width);
        layout.widths = widths;
        layout
    }

    #[test]
    fn closing_browser_preserves_chat_width_and_screen_position() {
        let mut layout = seeded([false, true, true]);
        let resize = layout
            .update([false, false, true], [420., 930., 386.], 1316.)
            .expect("resize");
        assert_eq!(resize.width, 386.);
        assert_eq!(resize.origin_offset, 930.);
        assert_eq!(layout.width(CHAT), 386.);
        layout.update([false, false, true], [420., 930., 386.], 1316.);
        layout.record_rendered_widths([false, false, true], &[1316.], 1316.);
        assert_eq!(layout.width(CHAT), 386.);
        layout.update([false, false, true], [420., 930., 386.], 386.);
        let reopen = layout
            .update([false, true, true], [420., 930., 386.], 386.)
            .expect("resize");
        assert_eq!(reopen.width, 1316.);
        assert_eq!(reopen.origin_offset, -930.);
    }

    #[test]
    fn toggling_each_panel_preserves_every_surviving_width() {
        for mask in 0u8..8 {
            let visible = [mask & 1 != 0, mask & 2 != 0, mask & 4 != 0];
            for panel in 0..3 {
                let mut layout = seeded(visible);
                let before = layout.widths;
                let mut after = visible;
                after[panel] = !after[panel];
                let resize = layout
                    .update(after, before, layout.viewport_width)
                    .expect("resize");
                for index in 0..3 {
                    assert_eq!(layout.width(index), before[index]);
                }
                if after.iter().any(|open| *open) {
                    assert_eq!(
                        resize.width,
                        (0..3)
                            .filter(|&index| after[index])
                            .map(|index| before[index])
                            .sum::<f32>()
                    );
                }
            }
        }
    }

    #[test]
    fn manual_resize_and_divider_changes_are_remembered() {
        let mut layout = seeded([false, true, true]);
        layout.update([false, true, true], [420., 700., 386.], 1200.);
        assert_eq!(layout.width(CHAT), 500.);
        let resize = layout
            .update([false, true, false], [420., 700., 386.], 1200.)
            .expect("resize");
        assert_eq!(resize.width, 700.);
        layout.update([false, true, false], [420., 700., 386.], 1000.);
        assert_eq!(layout.width(BROWSER), 700.);
        layout.update([false, true, false], [420., 700., 386.], 700.);
        layout.update([false, true, false], [420., 700., 386.], 900.);
        assert_eq!(layout.width(BROWSER), 900.);
        let resize = layout
            .update([false, true, true], [420., 700., 386.], 900.)
            .expect("resize");
        assert_eq!(resize.width, 1400.);
    }

    #[test]
    fn left_window_edge_resizes_browser_without_compressing_chat() {
        let visible = [false, true, true];
        let mut layout = seeded(visible);
        assert!(layout.sync_window_resize(visible, 100., 1316.).is_none());
        let widths = layout
            .sync_window_resize(visible, 980., 436.)
            .expect("resize");
        assert_eq!(widths[BROWSER], 50.);
        assert_eq!(widths[CHAT], 386.);
        layout.update(visible, widths, 436.);
        let widths = layout
            .sync_window_resize(visible, 100., 1316.)
            .expect("resize");
        assert_eq!(widths[BROWSER], 930.);
        assert_eq!(widths[CHAT], 386.);
    }

    #[test]
    fn right_window_edge_does_not_resize_browser_or_notes() {
        let visible = [true, true, true];
        let mut layout = seeded(visible);
        layout.sync_window_resize(visible, 100., 1736.);
        let widths = layout
            .sync_window_resize(visible, 100., 1936.)
            .expect("resize");
        assert_eq!(widths, [420., 930., 586.]);
    }

    #[test]
    fn browser_alone_resizes_from_either_window_edge() {
        let visible = [false, true, false];
        let mut layout = seeded(visible);
        layout.sync_window_resize(visible, 100., 930.);
        let widths = layout
            .sync_window_resize(visible, 500., 530.)
            .expect("left resize");
        assert_eq!(widths[BROWSER], 530.);
        layout.update(visible, widths, 530.);
        let widths = layout
            .sync_window_resize(visible, 500., 750.)
            .expect("right resize");
        assert_eq!(widths[BROWSER], 750.);
    }

    #[test]
    fn either_browser_divider_can_reduce_browser_to_fifty_pixels() {
        let mut left = seeded([true, true, true]);
        let widths = left.resize_divider(NOTES, 1300.).expect("left divider");
        assert_eq!(widths, [1300., 50., 386.]);
        let widths = left
            .resize_divider(NOTES, 420.)
            .expect("reverse left divider");
        assert_eq!(widths, [420., 930., 386.]);
        let mut right = seeded([true, true, true]);
        let widths = right.resize_divider(BROWSER, 50.).expect("right divider");
        assert_eq!(widths, [420., 50., 1266.]);
        let widths = right
            .resize_divider(BROWSER, 930.)
            .expect("reverse right divider");
        assert_eq!(widths, [420., 930., 386.]);
    }

    #[test]
    fn divider_moves_preserve_the_other_columns_and_respect_minima() {
        let mut layout = seeded([true, true, true]);
        assert_eq!(
            layout.resize_divider(NOTES, 10000.),
            Some([1300., 50., 386.])
        );
        assert_eq!(
            layout.resize_divider(NOTES, -100.),
            Some([280., 1070., 386.])
        );
        assert_eq!(
            layout.resize_divider(BROWSER, 10000.),
            Some([280., 1156., 300.])
        );
        assert_eq!(
            layout.resize_divider(BROWSER, -100.),
            Some([280., 50., 1406.])
        );
    }

    #[test]
    fn small_windows_keep_the_browser_minimum_when_resizing_either_edge() {
        let visible = [false, true, true];
        for origin in [100., 1186.] {
            let mut layout = seeded(visible);
            layout.sync_window_resize(visible, 100., 1316.);
            let widths = layout
                .sync_window_resize(visible, origin, 230.)
                .expect("shrink");
            assert_eq!(widths[BROWSER], 50.);
            assert_eq!(widths[CHAT], 180.);
            layout.update(visible, widths, 230.);
            let widths = layout
                .sync_window_resize(visible, origin, 280.)
                .expect("grow");
            assert_eq!(widths[BROWSER], 50.);
            assert_eq!(widths[CHAT], 230.);
        }
    }

    #[test]
    fn native_animation_bounds_and_window_moves_are_not_manual_resizes() {
        let visible = [false, true, true];
        let mut layout = seeded(visible);
        layout.sync_window_resize(visible, 100., 1316.);
        assert!(layout.sync_window_resize(visible, 300., 1316.).is_none());
        assert_eq!(layout.width(BROWSER), 930.);
        layout.update([false, false, true], [420., 930., 386.], 1316.);
        assert!(
            layout
                .sync_window_resize([false, false, true], 1230., 386.)
                .is_none()
        );
        assert_eq!(layout.width(BROWSER), 930.);
        assert_eq!(layout.width(CHAT), 386.);
        assert!(layout.resize_divider(BROWSER, 50.).is_none());
    }

    #[test]
    fn rapid_toggles_do_not_measure_intermediate_window_sizes() {
        let mut layout = seeded([true, true, true]);
        let resize = layout
            .update([true, false, true], [420., 930., 386.], 1736.)
            .expect("resize");
        assert_eq!(layout.resize_origin(100., resize.origin_offset, None), 100.);
        let resize = layout
            .update([false, false, true], [420., 930., 386.], 1736.)
            .expect("resize");
        assert_eq!(resize.width, 386.);
        assert_eq!(layout.width(CHAT), 386.);
        assert_eq!(layout.resize_origin(100., resize.origin_offset, None), 520.);
        let resize = layout
            .update([false, true, true], [420., 930., 386.], 1736.)
            .expect("resize");
        assert_eq!(
            layout.resize_origin(100., resize.origin_offset, None),
            -410.
        );
    }

    #[test]
    fn window_decorations_and_manual_moves_are_preserved() {
        let mut layout = seeded([false, true, true]);
        layout.record_rendered_widths([false, true, true], &[920., 376.], 1316.);
        let resize = layout
            .update([false, false, true], [420., 920., 376.], 1316.)
            .expect("resize");
        assert_eq!(resize.width, 396.);
        assert_eq!(
            layout.resize_origin(100., resize.origin_offset, Some((0., 1400.))),
            1020.
        );
        layout.update([false, false, true], [420., 920., 376.], 396.);
        let resize = layout
            .update([false, true, true], [420., 920., 376.], 396.)
            .expect("resize");
        assert_eq!(resize.width, 1316.);
        assert_eq!(
            layout.resize_origin(950., resize.origin_offset, Some((0., 500.))),
            30.
        );
    }

    #[test]
    fn restoring_a_session_does_not_resize_the_window() {
        let mut layout = PanelLayout::default();
        assert!(
            layout
                .update([false, true, true], [420., 700., 400.], 1100.)
                .is_none()
        );
        assert!(!layout.resizing());
        assert_eq!(layout.width(CHAT), 400.);
    }

    #[test]
    fn animation_keeps_chat_stationary_and_preserves_its_width() {
        let animation = PanelAnimation {
            from_origin: 100.,
            from_width: 1316.,
            to_origin: 1030.,
            to_width: 386.,
            widths: [420., 930., 386.],
            from_widths: [0., 930., 386.],
            visible: [false, false, true],
        };
        for step in 0..=20 {
            let progress = step as f32 / 20.;
            let frame = animation.frame(progress, 100.);
            assert_eq!(frame.widths[CHAT], 386.);
            assert_eq!(frame.positions[CHAT], 1030.);
            assert!((100. + frame.offsets[CHAT] + frame.widths[BROWSER] - 1030.).abs() < 0.01);
            assert!((frame.origin + frame.width - 1416.).abs() < 0.01);
            assert_eq!(frame.visible[BROWSER], step < 20);
            assert!((frame.positions[CHAT] - frame.origin - frame.widths[BROWSER]).abs() < 0.01);
            if step < 20 {
                assert!(
                    frame.widths[BROWSER] > 0.,
                    "closing browser must remain visible"
                );
            }
        }
    }

    #[test]
    fn opening_animation_reveals_browser_without_covering_chat() {
        let animation = PanelAnimation {
            from_origin: 1030.,
            from_width: 386.,
            to_origin: 100.,
            to_width: 1316.,
            widths: [420., 930., 386.],
            from_widths: [0., 0., 386.],
            visible: [false, true, true],
        };
        for step in 0..=20 {
            let progress = step as f32 / 20.;
            let frame = animation.frame(progress, 1030.);
            assert_eq!(frame.widths[CHAT], 386.);
            assert_eq!(frame.positions[CHAT], 1030.);
            assert!((frame.positions[BROWSER] + frame.widths[BROWSER] - 1030.).abs() < 0.01);
        }
        assert_eq!(animation.frame(1., 100.).widths[BROWSER], 930.);
    }

    #[test]
    fn animated_panels_never_overlap_and_finish_at_the_remembered_sizes() {
        for mask in 0u8..8 {
            let previous = [mask & 1 != 0, mask & 2 != 0, mask & 4 != 0];
            for panel in 0..3 {
                let mut layout = seeded(previous);
                let from_width = layout.viewport_width;
                let mut visible = previous;
                visible[panel] = !visible[panel];
                let resize = layout
                    .update(visible, layout.widths, from_width)
                    .expect("resize");
                let animation = PanelAnimation {
                    from_origin: 100.,
                    from_width,
                    to_origin: 100. + resize.origin_offset,
                    to_width: resize.width,
                    widths: layout.widths,
                    from_widths: std::array::from_fn(|index| {
                        if previous[index] {
                            layout.widths[index]
                        } else {
                            0.
                        }
                    }),
                    visible,
                };
                for step in 0..=20 {
                    let frame = animation.frame(step as f32 / 20., 100.);
                    let mut previous_right = None;
                    for index in 0..3 {
                        if !frame.visible[index] {
                            continue;
                        }
                        if let Some(right) = previous_right {
                            assert!(frame.positions[index] + 0.01 >= right);
                        }
                        previous_right = Some(frame.positions[index] + frame.widths[index]);
                        if (previous[index] && visible[index]) || step == 20 {
                            assert_eq!(frame.widths[index], animation.widths[index]);
                        }
                    }
                }
                let final_frame = animation.frame(1., 100.);
                assert_eq!(final_frame.origin, animation.to_origin);
                assert_eq!(final_frame.width, animation.to_width);
            }
        }
    }

    #[test]
    fn reversing_a_close_starts_at_the_last_visible_frame() {
        let closing = PanelAnimation {
            from_origin: 100.,
            from_width: 1316.,
            to_origin: 1030.,
            to_width: 386.,
            widths: [420., 930., 386.],
            from_widths: [0., 930., 386.],
            visible: [false, false, true],
        };
        let interrupted = closing.frame(0.4, 100.);
        let reopening = PanelAnimation {
            from_origin: interrupted.origin,
            from_width: interrupted.width,
            from_widths: interrupted.widths,
            to_origin: 100.,
            to_width: 1316.,
            widths: closing.widths,
            visible: [false, true, true],
        };
        let first = reopening.frame(0., 100.);
        assert_eq!(first.origin, interrupted.origin);
        assert_eq!(first.width, interrupted.width);
        assert_eq!(first.widths, interrupted.widths);
        assert_eq!(first.positions, interrupted.positions);
        let mut browser_width = first.widths[BROWSER];
        for step in 1..=20 {
            let frame = reopening.frame(step as f32 / 20., 100.);
            assert!(frame.visible[BROWSER]);
            assert!(frame.widths[BROWSER] >= browser_width);
            assert!((frame.positions[CHAT] - 1030.).abs() < 0.01);
            browser_width = frame.widths[BROWSER];
        }
    }
}
