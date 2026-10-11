use anyhow::{Context as _, Result, anyhow};
use collections::{HashMap, HashSet};
use futures::channel::oneshot;
#[cfg(not(test))]
use gpui::AppContext as _;
use gpui::{Context, Pixels, RenderImage, SharedString, SvgRenderer, Task, px};
use std::sync::{Arc, OnceLock, mpsc};

use crate::{Markdown, MarkdownStyle, parser::MarkdownEvent};

#[derive(Clone, Hash, PartialEq, Eq)]
struct Formula {
    source: SharedString,
    display: bool,
}

struct MathRequest {
    formula: Formula,
    response: oneshot::Sender<Result<String>>,
}

fn math_worker() -> Result<&'static mpsc::Sender<MathRequest>> {
    static WORKER: OnceLock<Result<mpsc::Sender<MathRequest>, String>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::channel::<MathRequest>();
            // MathJax owns a thread-local V8 isolate. Keep it on one worker instead
            // of initializing an isolate on each UI or background executor thread.
            std::thread::Builder::new()
                .name("markdown-math".into())
                .spawn(move || {
                    for request in receiver {
                        let result = if request.formula.display {
                            mathjax_svg::convert_to_svg(&request.formula.source)
                        } else {
                            mathjax_svg::convert_to_svg_inline(&request.formula.source)
                        }
                        .map_err(anyhow::Error::from);
                        if request.response.send(result).is_err() {
                            log::debug!("Math rendering request was cancelled");
                        }
                    }
                })
                .map(|_| sender)
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| anyhow!("Could not start the math renderer: {error}"))
}

async fn formula_svg(formula: Formula) -> Result<String> {
    let (sender, receiver) = oneshot::channel();
    math_worker()?
        .send(MathRequest {
            formula,
            response: sender,
        })
        .map_err(|_| anyhow!("The math renderer stopped"))?;
    receiver.await.context("Math rendering was interrupted")?
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct RenderKey {
    formula: Formula,
    color: u32,
    font_size: u32,
    scale: u32,
}

#[derive(Clone)]
pub(crate) struct MathImage {
    pub image: Arc<RenderImage>,
    pub width: Pixels,
}

struct CachedMath {
    image: Arc<OnceLock<Result<MathImage>>>,
    _task: Task<()>,
}

#[derive(Default)]
pub(crate) struct MathState {
    cache: HashMap<RenderKey, CachedMath>,
}

impl MathState {
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    pub fn retain(&mut self, events: &[(std::ops::Range<usize>, MarkdownEvent)]) {
        let formulas: HashSet<_> = events
            .iter()
            .filter_map(|(_, event)| match event {
                MarkdownEvent::Math { source, display } => Some(Formula {
                    source: source.clone(),
                    display: *display,
                }),
                _ => None,
            })
            .collect();
        self.cache.retain(|key, _| formulas.contains(&key.formula));
    }

    pub fn image(
        &mut self,
        source: SharedString,
        display: bool,
        style: &MarkdownStyle,
        font_size: Pixels,
        scale: f32,
        cx: &mut Context<Markdown>,
    ) -> Option<MathImage> {
        let key = RenderKey {
            formula: Formula { source, display },
            color: style.base_text_style.color.to_rgb().into(),
            font_size: f32::from(font_size).to_bits(),
            scale: scale.to_bits(),
        };
        let cached = self.cache.entry(key.clone()).or_insert_with(|| {
            let image = Arc::new(OnceLock::new());
            let completed = image.clone();
            let svg_renderer = cx.svg_renderer();
            #[cfg(test)]
            let task = {
                // An OS-thread waker cannot drive GPUI's deterministic test scheduler.
                // Complete the same worker and rasterizer before the test lays out the image.
                let result = futures::executor::block_on(formula_svg(key.formula.clone()))
                    .and_then(|svg| rasterize(&svg, &key, &svg_renderer));
                if completed.set(result).is_err() {
                    log::error!("Math rendering completed more than once");
                }
                Task::ready(())
            };
            #[cfg(not(test))]
            let task = cx.spawn(async move |this, cx| {
                let result = cx
                    .background_spawn(async move {
                        let svg = formula_svg(key.formula.clone()).await?;
                        rasterize(&svg, &key, &svg_renderer)
                    })
                    .await;
                if let Err(error) = &result {
                    log::debug!("Could not render a formula: {error:#}");
                }
                if completed.set(result).is_err() {
                    log::error!("Math rendering completed more than once");
                }
                if let Err(error) = this.update(cx, |_, cx| cx.notify()) {
                    log::debug!("Math rendering view was closed: {error:#}");
                }
            });
            CachedMath { image, _task: task }
        });
        cached
            .image
            .get()
            .and_then(|result| result.as_ref().ok())
            .cloned()
    }
}

fn rasterize(svg: &str, key: &RenderKey, renderer: &SvgRenderer) -> Result<MathImage> {
    let font_size = f32::from_bits(key.font_size);
    let scale = f32::from_bits(key.scale);
    let tree = resvg::usvg::Tree::from_str(
        svg,
        &resvg::usvg::Options {
            font_size,
            ..Default::default()
        },
    )?;
    let width = tree.size().width();
    let height = tree.size().height();
    anyhow::ensure!(
        width * height * scale * scale <= 8_000_000.0,
        "The formula is too large to render"
    );
    // Inherit the chat's text color and font size, including SVG's ex units.
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" font-size="{font_size}" color="#{:06x}" fill="#{:06x}">{svg}</svg>"##,
        key.color >> 8,
        key.color >> 8,
    );
    let image = renderer
        .render_single_frame(svg.as_bytes(), scale)
        .map_err(|error| anyhow!("Could not rasterize a formula: {error}"))?;
    Ok(MathImage {
        image,
        width: px(width),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn math_renders_common_tex_constructs_with_chat_color() -> Result<()> {
        let renderer = SvgRenderer::new(Arc::new(()));
        for source in [
            r"\frac{1+\sqrt{5}}{2}",
            r"\sum_{i=1}^{n}i^2=\frac{n(n+1)(2n+1)}{6}",
            r"\int_0^\infty e^{-x^2}\,dx=\frac{\sqrt{\pi}}{2}",
            r"\begin{pmatrix}a&b\\c&d\end{pmatrix}",
            r"f(x)=\begin{cases}x^2&x\ge0\\-x&x<0\end{cases}",
            r"\begin{aligned}x+y&=2\\100x+300y&=250\end{aligned}",
            r"P(\text{spam}\mid W_1\cap W_2)=\boxed{80\%}",
            r"\lim_{x\to0}\frac{\sin x}{x}=1",
        ] {
            let formula = Formula {
                source: source.to_owned().into(),
                display: true,
            };
            let (sender, receiver) = oneshot::channel();
            math_worker()?.send(MathRequest {
                formula: formula.clone(),
                response: sender,
            })?;
            let svg = futures::executor::block_on(receiver)??;
            for color in [0xffffffff, 0x202020ff] {
                let key = RenderKey {
                    formula: formula.clone(),
                    color,
                    font_size: 16.0f32.to_bits(),
                    scale: 1.0f32.to_bits(),
                };
                let image = rasterize(&svg, &key, &renderer)?;
                let bytes = image.image.as_bytes(0).context("No rendered pixels")?;
                assert!(image.width > px(0.0));
                assert!(bytes.chunks_exact(4).any(|pixel| pixel[3] > 0));
                if color == 0xffffffff {
                    assert!(
                        bytes.chunks_exact(4).any(|pixel| {
                            pixel[3] > 200 && pixel[..3].iter().all(|channel| *channel > 200)
                        }),
                        "Formula lost the light chat color: {source}"
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn math_invalid_tex_returns_error_without_discarding_source() -> Result<()> {
        let (sender, receiver) = oneshot::channel();
        math_worker()?.send(MathRequest {
            formula: Formula {
                source: r"\frac{1".into(),
                display: false,
            },
            response: sender,
        })?;
        assert!(futures::executor::block_on(receiver)?.is_err());
        Ok(())
    }

    #[test]
    fn math_worker_can_render_concurrently_with_notes() -> Result<()> {
        let note = std::thread::spawn(|| mathjax_svg::convert_to_svg(r"\sqrt{x^2+y^2}"));
        let chat = futures::executor::block_on(formula_svg(Formula {
            source: r"\frac{1}{2}".into(),
            display: false,
        }))?;
        let note = note
            .join()
            .map_err(|_| anyhow!("Note math renderer panicked"))??;
        assert!(chat.contains("<svg") && note.contains("<svg"));
        Ok(())
    }
}
