//! Blocos "ricos": a fonte fica no markdown (fenced code ```lang) e é renderizada
//! pra imagem (SVG → rasteriza no `render_block`). A fonte round-trips como .md.
//!
//! v1 (Fase 0+2 do plano): só `plot` — gráfico de função, 100% Rust → SVG, sem deps
//! pesadas. ponytail: math (```math) e mermaid entram como novos braços de
//! `render_rich_svg` (mesmo pipeline SVG→imagem); ver momor-editor-plan.md.

use std::collections::BTreeMap;

/// Langs de code block que renderizam pra imagem em vez de mostrar só o código-fonte.
pub fn is_rich_lang(lang: &str) -> bool {
    matches!(lang, "plot" | "math")
}

/// Fator de escala na rasterização (equações têm tamanho intrínseco pequeno em `ex`).
pub fn rich_scale(lang: &str) -> f32 {
    match lang {
        "math" => 3.0,
        _ => 1.0,
    }
}

/// SVG de um bloco rico a partir da fonte. `None` se lang não-rica ou fonte inválida
/// (o chamador cai de volta em mostrar só o code block — degradação graciosa).
pub fn render_rich_svg(lang: &str, source: &str) -> Option<String> {
    match lang {
        "plot" => render_plot_svg(source),
        // Equações LaTeX → SVG via MathJax (paths, não texto → resvg rasteriza direto).
        "math" => {
            let src = source.trim();
            (!src.is_empty())
                .then(|| mathjax_svg::convert_to_svg(src).ok())
                .flatten()
        }
        _ => None,
    }
}

const W: f64 = 640.0;
const H: f64 = 360.0;
const MARGIN: f64 = 28.0;
const SAMPLES: usize = 240;
const COLORS: &[&str] = &["#4f8cff", "#ff6b6b", "#2bbf7a", "#c084fc", "#f5a623"];

/// `plot`: cada linha não-vazia é uma função de `x` (ex.: `sin(x)`, `y = x^2`).
/// Uma linha `x: a..b` define o intervalo (default `-10..10`).
///
/// ponytail: `ez_eval` reparse a expressão por ponto (240×). É micro-segundos e o
/// resultado é cacheado por hash no render_block, então não vale otimizar agora.
/// Upgrade: `fasteval` compilado (`eval_compiled!`) se surgir plot pesado.
fn render_plot_svg(source: &str) -> Option<String> {
    let mut xmin = -10.0_f64;
    let mut xmax = 10.0_f64;
    let mut exprs: Vec<String> = Vec::new();

    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("x:") {
            if let Some((a, b)) = rest.split_once("..")
                && let (Ok(a), Ok(b)) = (a.trim().parse::<f64>(), b.trim().parse::<f64>())
            {
                xmin = a;
                xmax = b;
            }
            continue;
        }
        // Aceita "y = expr" ou só "expr".
        let expr = line
            .strip_prefix("y=")
            .or_else(|| line.strip_prefix("y ="))
            .map(str::trim)
            .unwrap_or(line);
        exprs.push(expr.to_string());
    }

    if exprs.is_empty() || !(xmax > xmin) {
        return None;
    }

    // Amostra cada função; acumula o range de y (ignorando não-finitos).
    let mut series: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut ymin = f64::INFINITY;
    let mut ymax = f64::NEG_INFINITY;
    let mut ns: BTreeMap<String, f64> = BTreeMap::new();

    for expr in &exprs {
        let mut pts = Vec::with_capacity(SAMPLES);
        let mut any_finite = false;
        for i in 0..SAMPLES {
            let x = xmin + (xmax - xmin) * (i as f64) / ((SAMPLES - 1) as f64);
            ns.insert("x".to_string(), x);
            let y = fasteval::ez_eval(expr, &mut ns).ok().unwrap_or(f64::NAN);
            if y.is_finite() {
                ymin = ymin.min(y);
                ymax = ymax.max(y);
                any_finite = true;
            }
            pts.push((x, y));
        }
        if any_finite {
            series.push(pts);
        }
    }

    if series.is_empty() || !ymin.is_finite() || !ymax.is_finite() {
        return None;
    }
    // Padding vertical; linha reta (ymin==ymax) vira uma faixa de ±1.
    if (ymax - ymin).abs() < 1e-9 {
        ymin -= 1.0;
        ymax += 1.0;
    } else {
        let pad = (ymax - ymin) * 0.08;
        ymin -= pad;
        ymax += pad;
    }

    let sx = |x: f64| MARGIN + (x - xmin) / (xmax - xmin) * (W - 2.0 * MARGIN);
    let sy = |y: f64| (H - MARGIN) - (y - ymin) / (ymax - ymin) * (H - 2.0 * MARGIN);

    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {W} {H}\">"
    ));
    // Fundo (card escuro sutil).
    svg.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{W}\" height=\"{H}\" rx=\"8\" fill=\"#0e1420\"/>"
    ));
    // Eixos (quando 0 está no range).
    if xmin < 0.0 && xmax > 0.0 {
        let x0 = sx(0.0);
        svg.push_str(&format!(
            "<line x1=\"{x0:.1}\" y1=\"{MARGIN}\" x2=\"{x0:.1}\" y2=\"{:.1}\" stroke=\"#38445a\" stroke-width=\"1\"/>",
            H - MARGIN
        ));
    }
    if ymin < 0.0 && ymax > 0.0 {
        let y0 = sy(0.0);
        svg.push_str(&format!(
            "<line x1=\"{MARGIN}\" y1=\"{y0:.1}\" x2=\"{:.1}\" y2=\"{y0:.1}\" stroke=\"#38445a\" stroke-width=\"1\"/>",
            W - MARGIN
        ));
    }
    // Curvas.
    for (idx, pts) in series.iter().enumerate() {
        let color = COLORS[idx % COLORS.len()];
        let points: String = pts
            .iter()
            .filter(|(_, y)| y.is_finite())
            .map(|(x, y)| format!("{:.1},{:.1}", sx(*x), sy(*y)))
            .collect::<Vec<_>>()
            .join(" ");
        if points.is_empty() {
            continue;
        }
        svg.push_str(&format!(
            "<polyline fill=\"none\" stroke=\"{color}\" stroke-width=\"2\" points=\"{points}\"/>"
        ));
    }
    svg.push_str("</svg>");
    Some(svg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plots_a_simple_function() {
        let svg = render_plot_svg("y = x").expect("linear plot renderiza");
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("<polyline"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn plots_math_functions_and_range() {
        let svg = render_plot_svg("x: -3.14..3.14\nsin(x)\ncos(x)").expect("trig renderiza");
        // Duas séries → duas polylines.
        assert_eq!(svg.matches("<polyline").count(), 2);
    }

    #[test]
    fn empty_or_garbage_is_none() {
        assert!(render_plot_svg("").is_none());
        assert!(render_plot_svg("   \n  ").is_none());
        // Expressão sem ponto finito nenhum → None (degrada pro code block).
        assert!(render_plot_svg("1/0").is_none());
    }

    #[test]
    fn only_plot_lang_is_rich() {
        assert!(is_rich_lang("plot"));
        assert!(!is_rich_lang("rust"));
        assert!(render_rich_svg("rust", "fn main() {}").is_none());
    }
}
