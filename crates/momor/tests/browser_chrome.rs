#[allow(dead_code)]
#[path = "../src/browser_session.rs"]
mod browser_session;

#[allow(dead_code)]
#[path = "../src/browser_payments.rs"]
mod browser_payments;
#[allow(dead_code)]
#[path = "../../workspace/src/panel_layout.rs"]
mod panel_layout;

#[cfg(target_os = "windows")]
#[allow(dead_code)]
#[path = "../src/native_webview.rs"]
mod native_webview;

#[cfg(test)]
mod overlay_tests {
    use gpui::{
        AppContext as _, Context, Entity, IntoElement, Render, TestAppContext, Window, div,
        prelude::*, px,
    };
    use std::{cell::Cell, rc::Rc};

    #[gpui::test]
    fn browser_visibility_ignores_superseded_requests(cx: &mut TestAppContext) {
        let visibility = Rc::new(std::cell::RefCell::new(Vec::new()));
        cx.update(|cx| {
            let visibility = visibility.clone();
            workspace::set_browser_visibility_callback(
                Rc::new(move |visible, _| {
                    visibility.borrow_mut().push(visible);
                }),
                cx,
            );
        });
        visibility.borrow_mut().clear();
        cx.update(|cx| {
            workspace::set_browser_open(true, cx);
            workspace::set_browser_open(false, cx);
            workspace::set_browser_open(true, cx);
        });
        cx.run_until_parked();
        assert!(!visibility.borrow().is_empty());
        assert!(
            visibility.borrow().iter().all(|visible| *visible),
            "stale close must not hide a reopened browser"
        );
        visibility.borrow_mut().clear();
        cx.update(|cx| workspace::set_browser_open(false, cx));
        cx.run_until_parked();
        assert_eq!(*visibility.borrow(), vec![false]);
    }

    struct OverlayHost {
        token: Option<Entity<()>>,
    }

    impl Render for OverlayHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().children(self.token.as_ref().map(|token| {
                ui::begin_popover(token, window, cx);
                ui::native_surface_overlay(token.entity_id(), div().w(px(160.)).h(px(32.)))
            }))
        }
    }

    #[gpui::test]
    fn tooltip_regions_are_removed_when_the_overlay_is_released(cx: &mut TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| OverlayHost {
            token: Some(cx.new(|_| ())),
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let regions = ui::native_surface_occlusions(window, cx);
            assert_eq!(regions.len(), 1);
            assert_eq!(regions[0].size, gpui::size(px(160.), px(32.)));
        });
        let notifications = Rc::new(Cell::new(0));
        cx.update(|_, cx| {
            let notifications = notifications.clone();
            cx.observe_global::<ui::NativeSurfaceOcclusions>(move |_| {
                notifications.set(notifications.get() + 1);
            })
            .detach();
        });
        root.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert_eq!(
            notifications.get(),
            0,
            "Unchanged tooltip bounds must not keep refreshing the window"
        );
        root.update(cx, |root, cx| {
            root.token = None;
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| assert!(ui::native_surface_occlusions(window, cx).is_empty()));
    }
}

#[cfg(test)]
mod layout_tests {
    use super::panel_layout::{AnimationFrame, PanelAnimation};
    use gpui::{
        Bounds, Context, IntoElement, Pixels, Render, TestAppContext, Window, canvas, div, fill,
        prelude::*, px, rgb, size,
    };
    use std::{cell::RefCell, rc::Rc};

    struct AnimationHost {
        frame: AnimationFrame,
        current_origin: f32,
        widths: [f32; 3],
        bounds: Rc<RefCell<Vec<Bounds<Pixels>>>>,
        content_bounds: Rc<RefCell<[Option<Bounds<Pixels>>; 3]>>,
        painted_bounds: Rc<RefCell<[Option<Bounds<Pixels>>; 3]>>,
        titlebar_bounds: Rc<RefCell<Option<Bounds<Pixels>>>>,
    }

    impl Render for AnimationHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let visible_bounds = Bounds::new(
                gpui::point(px(self.frame.origin - self.current_origin), px(32.)),
                size(px(self.frame.width), px(468.)),
            );
            div()
                .flex()
                .flex_col()
                .size_full()
                .overflow_hidden()
                .child(
                    div()
                        .relative()
                        .left(visible_bounds.origin.x)
                        .w(px(self.frame.width))
                        .h(px(32.))
                        .flex_none()
                        .child(div().size_full())
                        .on_children_prepainted({
                            let titlebar_bounds = self.titlebar_bounds.clone();
                            move |children, _, _| {
                                *titlebar_bounds.borrow_mut() = children.first().copied()
                            }
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_hidden()
                        .children(
                            (0..3)
                                .filter(|&index| self.frame.visible[index])
                                .map(|index| {
                                    div()
                                .relative()
                                .left(px(self.frame.offsets[index]))
                                .w(px(self.frame.widths[index]))
                                .h_full()
                                .flex_none()
                                .when(index != super::panel_layout::CHAT, |this| this.border_r_1())
                                .overflow_hidden()
                                .child(
                                    div()
                                        .w(px(self.widths[index]
                                            - if index == super::panel_layout::CHAT {
                                                0.
                                            } else {
                                                1.
                                            }))
                                        .h_full()
                                        .child(
                                            canvas(|bounds, _, _| bounds, {
                                                let painted_bounds = self.painted_bounds.clone();
                                                move |_, bounds: Bounds<Pixels>, window, _| {
                                                    painted_bounds.borrow_mut()[index] =
                                                        Some(bounds.intersect(
                                                            &window.content_mask().bounds,
                                                        ).intersect(&visible_bounds));
                                                    window.paint_quad(fill(bounds, rgb(0x20a060)));
                                                }
                                            })
                                            .size_full(),
                                        ),
                                )
                                .on_children_prepainted({
                                    let content_bounds = self.content_bounds.clone();
                                    move |children, _, _| {
                                        content_bounds.borrow_mut()[index] =
                                            children.first().copied()
                                    }
                                })
                                }),
                        )
                        .on_children_prepainted({
                            let bounds = self.bounds.clone();
                            move |children, _, _| *bounds.borrow_mut() = children
                        }),
                )
        }
    }

    #[gpui::test]
    fn animated_layout_matches_the_real_gpui_panel_bounds(cx: &mut TestAppContext) {
        let bounds = Rc::new(RefCell::new(Vec::new()));
        let content_bounds = Rc::new(RefCell::new([None; 3]));
        let painted_bounds = Rc::new(RefCell::new([None; 3]));
        let titlebar_bounds = Rc::new(RefCell::new(None));
        let mut animations = vec![
            PanelAnimation {
                from_origin: 100.,
                from_width: 1316.,
                to_origin: 1030.,
                to_width: 386.,
                widths: [420., 930., 386.],
                from_widths: [0., 930., 386.],
                visible: [false, false, true],
            },
            PanelAnimation {
                from_origin: 1030.,
                from_width: 386.,
                to_origin: 100.,
                to_width: 1316.,
                widths: [420., 930., 386.],
                from_widths: [0., 0., 386.],
                visible: [false, true, true],
            },
            PanelAnimation {
                from_origin: 100.,
                from_width: 1736.,
                to_origin: 100.,
                to_width: 806.,
                widths: [420., 930., 386.],
                from_widths: [420., 930., 386.],
                visible: [true, false, true],
            },
        ];
        for mask in 1u8..8 {
            let previous = [mask & 1 != 0, mask & 2 != 0, mask & 4 != 0];
            let widths = [420., 930., 386.];
            let offset = |panels: [bool; 3], end: usize| {
                (0..end)
                    .filter(|&index| panels[index])
                    .map(|index| widths[index])
                    .sum::<f32>()
            };
            for panel in 0..3 {
                let mut visible = previous;
                visible[panel] = !visible[panel];
                let Some(anchor) = (0..3).find(|&index| previous[index] && visible[index]) else {
                    continue;
                };
                animations.push(PanelAnimation {
                    from_origin: 100.,
                    from_width: offset(previous, 3),
                    to_origin: 100. + offset(previous, anchor) - offset(visible, anchor),
                    to_width: offset(visible, 3),
                    widths,
                    from_widths: std::array::from_fn(|index| {
                        if previous[index] { widths[index] } else { 0. }
                    }),
                    visible,
                });
            }
        }
        let (root, cx) = cx.add_window_view(|_, _| AnimationHost {
            frame: animations[0].frame(0., 100.),
            current_origin: 100.,
            widths: animations[0].widths,
            bounds: bounds.clone(),
            content_bounds: content_bounds.clone(),
            painted_bounds: painted_bounds.clone(),
            titlebar_bounds: titlebar_bounds.clone(),
        });
        for animation in animations {
            for step in 0..=20 {
                let progress = step as f32 / 20.;
                let current_origin = if animation.to_width < animation.from_width {
                    animation.from_origin
                } else {
                    animation.to_origin
                };
                let native_width = if animation.to_width < animation.from_width {
                    animation.from_width
                } else {
                    animation.to_width
                };
                let expected = animation.frame(progress, current_origin);
                cx.simulate_resize(size(px(native_width), px(500.)));
                *painted_bounds.borrow_mut() = [None; 3];
                root.update(cx, |root, cx| {
                    root.frame = animation.frame(progress, current_origin);
                    root.current_origin = current_origin;
                    root.widths = animation.widths;
                    cx.notify();
                });
                cx.run_until_parked();
                let bounds = bounds.borrow();
                let titlebar = titlebar_bounds.borrow().expect("titlebar bounds");
                assert_eq!(titlebar.size.height, px(32.));
                assert!((f32::from(titlebar.size.width) - expected.width).abs() <= 1.);
                assert!(
                    (f32::from(titlebar.origin.x) + current_origin - expected.origin).abs() <= 1.
                );
                let mut actual = bounds.iter();
                let mut painted_right = expected.origin;
                for index in 0..3 {
                    if !expected.visible[index] {
                        assert!(
                            painted_bounds.borrow()[index].is_none(),
                            "closed panel must be unmounted"
                        );
                        continue;
                    }
                    let actual = actual.next().expect("visible panel bounds");
                    assert_eq!(actual.origin.y, px(32.), "panel must remain below titlebar");
                    assert_eq!(
                        actual.size.height,
                        px(468.),
                        "panel must not disappear vertically"
                    );
                    assert!(
                        (f32::from(actual.size.width) - expected.widths[index]).abs() <= 1.,
                        "panel {index}, step {step}: {actual:?}, expected width {}",
                        expected.widths[index]
                    );
                    let content = content_bounds.borrow()[index].expect("panel content bounds");
                    assert_eq!(content.size.height, px(468.));
                    assert_eq!(
                        f32::from(content.size.width),
                        animation.widths[index]
                            - if index == super::panel_layout::CHAT {
                                0.
                            } else {
                                1.
                            }
                    );
                    assert!(
                        (f32::from(actual.origin.x) + current_origin - expected.positions[index])
                            .abs()
                            <= 1.,
                        "panel {index}, step {step}: {actual:?}, expected x {}",
                        expected.positions[index] - current_origin
                    );
                    let painted = painted_bounds.borrow()[index].expect("visible panel must paint");
                    assert!(
                        (f32::from(painted.origin.x) + current_origin - painted_right).abs() <= 2.,
                        "blank strip before panel {index} at step {step}: {painted:?}"
                    );
                    painted_right = f32::from(painted.right()) + current_origin;
                    assert!(
                        (f32::from(painted.size.width) - expected.widths[index]).abs() <= 2.,
                        "panel {index} disappeared before animation ended at step {step}: {painted:?}"
                    );
                    if animation.from_widths[index] > 0. && animation.visible[index] {
                        let painted =
                            painted_bounds.borrow()[index].expect("surviving panel must paint");
                        assert_eq!(painted.size.height, px(468.));
                        assert!(
                            f32::from(painted.size.width) >= animation.widths[index] - 2.,
                            "surviving panel {index} vanished at step {step}: {painted:?}"
                        );
                        assert!(expected.positions[index] >= current_origin);
                        assert!(
                            expected.positions[index] + expected.widths[index]
                                <= current_origin + native_width
                        );
                    }
                }
                assert!(
                    (painted_right - expected.origin - expected.width).abs() <= 2.,
                    "visible window contains an empty strip at step {step}"
                );
            }
        }
    }
}
