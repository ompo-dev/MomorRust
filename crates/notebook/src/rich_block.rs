//! Blocos "ricos": a fonte fica no markdown (fenced code ```lang) e é renderizada
//! pra imagem (SVG → rasteriza no `render_block`). A fonte round-trips como .md.
//!
//! v1: `plot` e `math`.
//! v2 ponytail: `mathstudio` reaproveita o mesmo pipeline SVG->imagem, com uma DSL
//! segura inspirada no Code-Mentor em vez de executar JavaScript arbitrário.

use std::{collections::BTreeMap, fmt::Write as _};

#[derive(Clone, Copy, Debug)]
pub struct MathStudioView {
    pub zoom: f64,
    pub pan_x: f64,
    pub pan_y: f64,
    pub orbit_x: f64,
    pub orbit_y: f64,
    pub time: f64,
}

impl Default for MathStudioView {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            orbit_x: 0.0,
            orbit_y: 0.0,
            time: 0.0,
        }
    }
}

/// Valor de uma função no ponto atual do tooltip 2D.
#[derive(Clone, Debug, PartialEq)]
pub struct MathStudioHoverSample {
    pub label: Option<String>,
    pub y: f64,
}

/// Coordenadas e amostras exibidas no tooltip 2D do Math Studio.
#[derive(Clone, Debug, PartialEq)]
pub struct MathStudioHover {
    pub world_x: f64,
    pub world_y: f64,
    pub samples: Vec<MathStudioHoverSample>,
}

/// Langs de code block que renderizam pra imagem em vez de mostrar só o código-fonte.
pub fn is_rich_lang(lang: &str) -> bool {
    matches!(
        lang.to_ascii_lowercase().as_str(),
        "plot" | "math" | "mathstudio"
    )
}

/// Fator de escala na rasterização (equações têm tamanho intrínseco pequeno em `ex`).
pub fn rich_scale(lang: &str) -> f32 {
    match lang.to_ascii_lowercase().as_str() {
        "math" => 3.0,
        _ => 1.0,
    }
}

/// SVG de um bloco rico a partir da fonte. `None` se lang não-rica ou fonte inválida
/// (o chamador cai de volta em mostrar só o code block — degradação graciosa).
pub fn render_rich_svg(lang: &str, source: &str) -> Option<String> {
    render_rich_svg_with_view(lang, source, MathStudioView::default())
}

pub fn render_rich_svg_with_view(lang: &str, source: &str, view: MathStudioView) -> Option<String> {
    match lang.to_ascii_lowercase().as_str() {
        "plot" => render_plot_svg(source),
        "mathstudio" => render_mathstudio_svg(source, view),
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
const MAX_STUDIO_STEPS: usize = 12_000;

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

#[derive(Clone)]
struct Function2D {
    expr: String,
    vars: BTreeMap<String, f64>,
    color: String,
    width: f64,
    label: Option<String>,
    domain: Option<(f64, f64)>,
    samples: usize,
}

#[derive(Clone)]
struct Parametric2D {
    x_expr: String,
    y_expr: String,
    vars: BTreeMap<String, f64>,
    t: (f64, f64),
    color: String,
    width: f64,
    samples: usize,
}

#[derive(Clone)]
struct Vector2D {
    from: (f64, f64),
    to: (f64, f64),
    color: String,
    label: Option<String>,
}

#[derive(Clone)]
struct Point2D {
    at: (f64, f64),
    color: String,
    radius: f64,
    label: Option<String>,
}

#[derive(Clone)]
struct Text2D {
    at: (f64, f64),
    text: String,
    color: String,
    size: f64,
}

#[derive(Clone)]
struct VectorField2D {
    x_expr: String,
    y_expr: String,
    vars: BTreeMap<String, f64>,
    density: usize,
    scale: f64,
    color: String,
}

#[derive(Clone)]
struct Surface3D {
    z_expr: String,
    vars: BTreeMap<String, f64>,
    x: (f64, f64),
    y: (f64, f64),
    segments: usize,
    color: String,
    opacity: f64,
}

#[derive(Clone)]
struct Curve3D {
    x_expr: String,
    y_expr: String,
    z_expr: String,
    vars: BTreeMap<String, f64>,
    t: (f64, f64),
    color: String,
    width: f64,
    samples: usize,
}

#[derive(Clone)]
struct Vector3D {
    from: (f64, f64, f64),
    to: (f64, f64, f64),
    color: String,
    label: Option<String>,
}

#[derive(Clone)]
struct Point3D {
    at: (f64, f64, f64),
    color: String,
    radius: f64,
    label: Option<String>,
}

#[derive(Clone)]
struct Particles3D {
    positions: Vec<(f64, f64, f64)>,
    color: String,
    size: f64,
}

#[derive(Clone)]
struct VectorField3D {
    x_expr: String,
    y_expr: String,
    z_expr: String,
    vars: BTreeMap<String, f64>,
    bounds: (f64, f64),
    density: usize,
    scale: f64,
    color: String,
}

#[derive(Default)]
struct MathStudioScene {
    background: String,
    x: (f64, f64),
    y: (f64, f64),
    step: f64,
    grid: bool,
    axes: bool,
    functions: Vec<Function2D>,
    params: Vec<Parametric2D>,
    vectors: Vec<Vector2D>,
    points: Vec<Point2D>,
    texts: Vec<Text2D>,
    fields: Vec<VectorField2D>,
    is_3d: bool,
    axes_3d: bool,
    size_3d: f64,
    surfaces_3d: Vec<Surface3D>,
    curves_3d: Vec<Curve3D>,
    vectors_3d: Vec<Vector3D>,
    points_3d: Vec<Point3D>,
    particles_3d: Vec<Particles3D>,
    fields_3d: Vec<VectorField3D>,
    diagnostics: Vec<String>,
}

impl MathStudioScene {
    fn new() -> Self {
        Self {
            background: "#0d1117".into(),
            x: (-10.0, 10.0),
            y: (-6.0, 6.0),
            step: 1.0,
            grid: true,
            axes: true,
            axes_3d: true,
            size_3d: 4.0,
            ..Default::default()
        }
    }
}

fn render_mathstudio_svg(source: &str, view: MathStudioView) -> Option<String> {
    let source = source.trim();
    if source.is_empty() {
        return None;
    }
    let scene = parse_mathstudio(source, view.time);
    Some(scene_to_svg(&scene, view))
}

pub fn mathstudio_hover(
    source: &str,
    view: MathStudioView,
    local_x: f64,
    local_y: f64,
    width: f64,
    height: f64,
) -> Option<MathStudioHover> {
    if source.trim().is_empty() || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let scene = parse_mathstudio(source, view.time);
    if scene.is_3d {
        return None;
    }

    let zoom = view.zoom.clamp(0.2, 20.0);
    let base_x = scene.x.1 - scene.x.0;
    let base_y = scene.y.1 - scene.y.0;
    let x_span = base_x / zoom;
    let y_span = base_y / zoom;
    let x_center = (scene.x.0 + scene.x.1) * 0.5 - view.pan_x * base_x / zoom;
    let y_center = (scene.y.0 + scene.y.1) * 0.5 + view.pan_y * base_y / zoom;
    let view_x = (x_center - x_span * 0.5, x_center + x_span * 0.5);
    let view_y = (y_center - y_span * 0.5, y_center + y_span * 0.5);
    let svg_x = (local_x / width).clamp(0.0, 1.0) * W;
    let svg_y = (local_y / height).clamp(0.0, 1.0) * H;
    let plot_w = W - 2.0 * MARGIN;
    let plot_h = H - 2.0 * MARGIN;
    let world_x = view_x.0 + ((svg_x - MARGIN) / plot_w) * (view_x.1 - view_x.0);
    let world_y = view_y.1 - ((svg_y - MARGIN) / plot_h) * (view_y.1 - view_y.0);
    let samples = scene
        .functions
        .iter()
        .filter_map(|function| {
            eval_expr_at_x(&function.expr, world_x, &function.vars).map(|y| MathStudioHoverSample {
                label: function.label.clone(),
                y,
            })
        })
        .filter(|sample| sample.y.is_finite())
        .take(6)
        .collect::<Vec<_>>();

    Some(MathStudioHover {
        world_x,
        world_y,
        samples,
    })
}

fn parse_mathstudio(source: &str, time: f64) -> MathStudioScene {
    let mut scene = MathStudioScene::new();
    let mut vars = default_vars();
    let mut budget = MAX_STUDIO_STEPS;

    for stmt in split_statements(source) {
        let stmt = stmt.trim();
        if stmt.is_empty() || stmt.starts_with("//") {
            continue;
        }
        if stmt.starts_with("scene.animate") || stmt.starts_with("scene3d.animate") {
            apply_animation_stmt(stmt, time, &mut vars);
            continue;
        }
        if stmt.starts_with("log(") {
            continue;
        }
        if contains_forbidden(stmt) {
            scene
                .diagnostics
                .push("comando bloqueado pela DSL segura".into());
            continue;
        }
        if let Some(assignments) = parse_numeric_assignments(stmt, &vars) {
            for (name, value) in assignments {
                vars.insert(name, value);
            }
            continue;
        }
        if let Some(arg) = call_arg(stmt, "scene.background") {
            if let Some(color) = parse_string(arg) {
                scene.background = color;
            }
            continue;
        }
        if let Some(arg) = call_arg(stmt, "scene.cartesianPlane") {
            apply_plane(arg, &vars, &mut scene);
            continue;
        }
        if let Some(arg) = call_arg(stmt, "scene.function") {
            if let Some(function) = parse_function(arg, &vars, &mut budget) {
                scene.functions.push(function);
            }
            continue;
        }
        if let Some(arg) = call_arg(stmt, "scene.parametric") {
            if let Some(param) = parse_parametric(arg, &vars, &mut budget) {
                scene.params.push(param);
            }
            continue;
        }
        if let Some(arg) = call_arg(stmt, "scene.vectorField") {
            if let Some(field) = parse_vector_field(arg, &vars, &mut budget) {
                scene.fields.push(field);
            }
            continue;
        }
        if let Some(arg) = call_arg(stmt, "scene.vector") {
            if let Some(vector) = parse_vector(arg, &vars) {
                scene.vectors.push(vector);
            }
            continue;
        }
        if let Some(arg) = call_arg(stmt, "scene.point") {
            if let Some(point) = parse_point(arg, &vars) {
                scene.points.push(point);
            }
            continue;
        }
        if let Some(arg) = call_arg(stmt, "scene.text") {
            if let Some(text) = parse_text(arg, &vars) {
                scene.texts.push(text);
            }
            continue;
        }
        if let Some(arg) = call_arg(stmt, "scene3d.background") {
            scene.is_3d = true;
            if let Some(color) = parse_string(arg) {
                scene.background = color;
            }
            continue;
        }
        if let Some(arg) = call_arg(stmt, "scene3d.axes") {
            scene.is_3d = true;
            scene.axes_3d = prop(arg, "enabled").and_then(parse_bool).unwrap_or(true);
            if let Some(size) = prop(arg, "size").and_then(|v| eval_expr(v, &vars).ok())
                && size.is_finite()
                && size > 0.0
            {
                scene.size_3d = size.clamp(1.0, 20.0);
            }
            continue;
        }
        if let Some(arg) = call_arg(stmt, "scene3d.surface") {
            scene.is_3d = true;
            if let Some(surface) = parse_surface_3d(arg, &vars, &mut budget) {
                scene.surfaces_3d.push(surface);
            }
            continue;
        }
        if let Some(arg) = call_arg(stmt, "scene3d.curve3D") {
            scene.is_3d = true;
            if let Some(curve) = parse_curve_3d(arg, &vars, &mut budget) {
                scene.curves_3d.push(curve);
            }
            continue;
        }
        if let Some(arg) = call_arg(stmt, "scene3d.vectorField3D") {
            scene.is_3d = true;
            if let Some(field) = parse_vector_field_3d(arg, &vars, &mut budget) {
                scene.fields_3d.push(field);
            }
            continue;
        }
        if let Some(arg) = call_arg(stmt, "scene3d.vector3D") {
            scene.is_3d = true;
            if let Some(vector) = parse_vector_3d(arg, &vars) {
                scene.vectors_3d.push(vector);
            }
            continue;
        }
        if let Some(arg) = call_arg(stmt, "scene3d.point3D") {
            scene.is_3d = true;
            if let Some(point) = parse_point_3d(arg, &vars) {
                scene.points_3d.push(point);
            }
            continue;
        }
        if let Some(arg) = call_arg(stmt, "scene3d.particles") {
            scene.is_3d = true;
            if let Some(particles) = parse_particles_3d(arg, &vars) {
                scene.particles_3d.push(particles);
            }
            continue;
        }
        if stmt.starts_with("scene3d.") {
            scene
                .diagnostics
                .push("preview 3D ainda não foi portado para o bloco nativo".into());
            continue;
        }
        scene
            .diagnostics
            .push(format!("ignorado pela DSL segura: {}", compact(stmt)));
    }

    scene
}

fn default_vars() -> BTreeMap<String, f64> {
    BTreeMap::from([
        ("pi".into(), std::f64::consts::PI),
        ("PI".into(), std::f64::consts::PI),
        ("e".into(), std::f64::consts::E),
        ("E".into(), std::f64::consts::E),
    ])
}

fn split_statements(source: &str) -> Vec<String> {
    let source = source
        .lines()
        .map(strip_line_comment)
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0_i32;
    let mut quote: Option<char> = None;
    let mut escape = false;

    for ch in source.chars() {
        if let Some(q) = quote {
            cur.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' | '`' => {
                quote = Some(ch);
                cur.push(ch);
            }
            '(' | '[' | '{' => {
                depth += 1;
                cur.push(ch);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                cur.push(ch);
            }
            ';' if depth <= 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn strip_line_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut escape = false;
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        let (idx, ch) = chars[i];
        if let Some(q) = quote {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match ch {
            '"' | '\'' | '`' => quote = Some(ch),
            '/' if chars.get(i + 1).is_some_and(|(_, next)| *next == '/') => return &line[..idx],
            _ => {}
        }
        i += 1;
    }
    line
}

fn contains_forbidden(stmt: &str) -> bool {
    [
        "import",
        "export",
        "require",
        "fetch",
        "XMLHttpRequest",
        "window",
        "document",
        "localStorage",
        "Function",
        "eval",
        "while",
        "for",
        "Promise",
        "async",
        "await",
    ]
    .iter()
    .any(|needle| stmt.contains(needle))
}

fn parse_numeric_assignments(
    stmt: &str,
    vars: &BTreeMap<String, f64>,
) -> Option<Vec<(String, f64)>> {
    let rest = stmt
        .strip_prefix("const ")
        .or_else(|| stmt.strip_prefix("let "))
        .or_else(|| stmt.strip_prefix("var "))?;
    let mut out = Vec::new();
    let mut local = vars.clone();
    for part in split_top_level(rest, ',') {
        let (name, expr) = part.split_once('=')?;
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        let expr = expr.trim();
        let value = eval_calc_call(expr, &local).or_else(|| eval_expr(expr, &local).ok())?;
        local.insert(name.to_string(), value);
        out.push((name.to_string(), value));
    }
    (!out.is_empty()).then_some(out)
}

fn apply_animation_stmt(stmt: &str, time: f64, vars: &mut BTreeMap<String, f64>) {
    let Some(arg) = call_arg(stmt, "scene.animate").or_else(|| call_arg(stmt, "scene3d.animate"))
    else {
        return;
    };
    let Some((_, body)) = arg.split_once("=>") else {
        return;
    };
    let body = body.trim().trim_start_matches('{').trim_end_matches('}');
    let mut local = vars.clone();
    local.insert("t".into(), time);
    local.insert("time".into(), time);
    for stmt in split_statements(body) {
        if let Some((name, value)) = parse_numeric_update(&stmt, &local) {
            local.insert(name.clone(), value);
            vars.insert(name, value);
        }
    }
}

fn parse_numeric_update(stmt: &str, vars: &BTreeMap<String, f64>) -> Option<(String, f64)> {
    let (name, expr) = stmt.split_once('=')?;
    if name.ends_with('+') || name.ends_with('-') || name.ends_with('*') || name.ends_with('/') {
        return None;
    }
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let value = eval_expr(expr.trim(), vars).ok()?;
    Some((name.to_string(), value))
}

fn call_arg<'a>(stmt: &'a str, name: &str) -> Option<&'a str> {
    let rest = stmt.trim().strip_prefix(name)?.trim_start();
    let rest = rest.strip_prefix('(')?;
    rest.strip_suffix(')').map(str::trim)
}

fn apply_plane(arg: &str, vars: &BTreeMap<String, f64>, scene: &mut MathStudioScene) {
    if let Some(x) = prop(arg, "x").and_then(|v| parse_pair(v, vars)) {
        scene.x = x;
    }
    if let Some(y) = prop(arg, "y").and_then(|v| parse_pair(v, vars)) {
        scene.y = y;
    }
    if let Some(step) = prop(arg, "step").and_then(|v| eval_expr(v, vars).ok()) {
        if step.is_finite() && step > 0.0 {
            scene.step = step;
        }
    }
    if let Some(grid) = prop(arg, "grid").and_then(parse_bool) {
        scene.grid = grid;
    }
    if let Some(axes) = prop(arg, "axes").and_then(parse_bool) {
        scene.axes = axes;
    }
}

fn parse_function(
    arg: &str,
    vars: &BTreeMap<String, f64>,
    budget: &mut usize,
) -> Option<Function2D> {
    let expr = prop(arg, "expression")
        .and_then(|v| parse_expr_value(v, "x"))
        .or_else(|| prop(arg, "expr").and_then(|v| parse_expr_value(v, "x")))?;
    let samples = bounded_samples(prop(arg, "samples"), vars, 32, 600, budget);
    Some(Function2D {
        expr,
        vars: vars.clone(),
        color: prop(arg, "color")
            .and_then(parse_string)
            .unwrap_or_else(|| COLORS[0].into()),
        width: prop(arg, "width")
            .and_then(|v| eval_expr(v, vars).ok())
            .unwrap_or(2.0)
            .clamp(0.5, 8.0),
        label: prop(arg, "label").and_then(parse_string),
        domain: prop(arg, "domain").and_then(|v| parse_pair(v, vars)),
        samples,
    })
}

fn parse_parametric(
    arg: &str,
    vars: &BTreeMap<String, f64>,
    budget: &mut usize,
) -> Option<Parametric2D> {
    let (x_expr, y_expr) = prop(arg, "fn")
        .and_then(|v| parse_arrow_array(v, "t"))
        .or_else(|| prop(arg, "expression").and_then(|v| parse_arrow_array(v, "t")))?;
    let samples = bounded_samples(prop(arg, "samples"), vars, 32, 700, budget);
    Some(Parametric2D {
        x_expr,
        y_expr,
        vars: vars.clone(),
        t: prop(arg, "t")
            .and_then(|v| parse_pair(v, vars))
            .unwrap_or((0.0, std::f64::consts::TAU)),
        color: prop(arg, "color")
            .and_then(parse_string)
            .unwrap_or_else(|| COLORS[3].into()),
        width: prop(arg, "width")
            .and_then(|v| eval_expr(v, vars).ok())
            .unwrap_or(2.0)
            .clamp(0.5, 8.0),
        samples,
    })
}

fn parse_vector_field(
    arg: &str,
    vars: &BTreeMap<String, f64>,
    budget: &mut usize,
) -> Option<VectorField2D> {
    let (x_expr, y_expr) = prop(arg, "fn").and_then(|v| parse_arrow_array(v, "x"))?;
    let density = bounded_samples(prop(arg, "density"), vars, 4, 24, budget);
    Some(VectorField2D {
        x_expr,
        y_expr,
        vars: vars.clone(),
        density,
        scale: prop(arg, "scale")
            .and_then(|v| eval_expr(v, vars).ok())
            .unwrap_or(0.35)
            .clamp(0.02, 3.0),
        color: prop(arg, "color")
            .and_then(parse_string)
            .unwrap_or_else(|| "rgba(96,165,250,0.55)".into()),
    })
}

fn parse_vector(arg: &str, vars: &BTreeMap<String, f64>) -> Option<Vector2D> {
    Some(Vector2D {
        from: prop(arg, "from").and_then(|v| parse_pair(v, vars))?,
        to: prop(arg, "to").and_then(|v| parse_pair(v, vars))?,
        color: prop(arg, "color")
            .and_then(parse_string)
            .unwrap_or_else(|| COLORS[2].into()),
        label: prop(arg, "label").and_then(parse_string),
    })
}

fn parse_point(arg: &str, vars: &BTreeMap<String, f64>) -> Option<Point2D> {
    Some(Point2D {
        at: prop(arg, "at").and_then(|v| parse_pair(v, vars))?,
        color: prop(arg, "color")
            .and_then(parse_string)
            .unwrap_or_else(|| COLORS[1].into()),
        radius: prop(arg, "radius")
            .and_then(|v| eval_expr(v, vars).ok())
            .unwrap_or(4.0)
            .clamp(1.0, 14.0),
        label: prop(arg, "label").and_then(parse_string),
    })
}

fn parse_text(arg: &str, vars: &BTreeMap<String, f64>) -> Option<Text2D> {
    Some(Text2D {
        at: prop(arg, "at").and_then(|v| parse_pair(v, vars))?,
        text: prop(arg, "text")
            .and_then(parse_string)
            .map(|text| interpolate_vars(&text, vars))?,
        color: prop(arg, "color")
            .and_then(parse_string)
            .unwrap_or_else(|| "#cbd5e1".into()),
        size: prop(arg, "size")
            .and_then(|v| eval_expr(v, vars).ok())
            .unwrap_or(14.0)
            .clamp(8.0, 36.0),
    })
}

fn parse_surface_3d(
    arg: &str,
    vars: &BTreeMap<String, f64>,
    budget: &mut usize,
) -> Option<Surface3D> {
    let z_expr = prop(arg, "fn")
        .and_then(|v| parse_arrow_expr(v, "x"))
        .or_else(|| prop(arg, "z").and_then(|v| parse_expr_value(v, "x")))?;
    Some(Surface3D {
        z_expr,
        vars: vars.clone(),
        x: prop(arg, "x")
            .and_then(|v| parse_pair(v, vars))
            .unwrap_or((-4.0, 4.0)),
        y: prop(arg, "y")
            .and_then(|v| parse_pair(v, vars))
            .unwrap_or((-4.0, 4.0)),
        segments: bounded_samples(prop(arg, "segments"), vars, 6, 28, budget),
        color: prop(arg, "color")
            .and_then(parse_string)
            .unwrap_or_else(|| COLORS[0].into()),
        opacity: prop(arg, "opacity")
            .and_then(|v| eval_expr(v, vars).ok())
            .unwrap_or(0.72)
            .clamp(0.1, 1.0),
    })
}

fn parse_curve_3d(arg: &str, vars: &BTreeMap<String, f64>, budget: &mut usize) -> Option<Curve3D> {
    let (x_expr, y_expr, z_expr) = prop(arg, "fn")
        .and_then(parse_arrow_array3)
        .or_else(|| prop(arg, "expression").and_then(parse_arrow_array3))?;
    Some(Curve3D {
        x_expr,
        y_expr,
        z_expr,
        vars: vars.clone(),
        t: prop(arg, "t")
            .and_then(|v| parse_pair(v, vars))
            .unwrap_or((0.0, std::f64::consts::TAU)),
        color: prop(arg, "color")
            .and_then(parse_string)
            .unwrap_or_else(|| COLORS[3].into()),
        width: prop(arg, "width")
            .and_then(|v| eval_expr(v, vars).ok())
            .unwrap_or(2.0)
            .clamp(0.5, 8.0),
        samples: bounded_samples(prop(arg, "samples"), vars, 32, 420, budget),
    })
}

fn parse_vector_field_3d(
    arg: &str,
    vars: &BTreeMap<String, f64>,
    budget: &mut usize,
) -> Option<VectorField3D> {
    let (x_expr, y_expr, z_expr) = prop(arg, "fn").and_then(parse_arrow_array3)?;
    Some(VectorField3D {
        x_expr,
        y_expr,
        z_expr,
        vars: vars.clone(),
        bounds: prop(arg, "bounds")
            .and_then(|v| parse_pair(v, vars))
            .unwrap_or((-3.0, 3.0)),
        density: bounded_samples(prop(arg, "density"), vars, 3, 5, budget),
        scale: prop(arg, "scale")
            .and_then(|v| eval_expr(v, vars).ok())
            .unwrap_or(0.25)
            .clamp(0.02, 2.0),
        color: prop(arg, "color")
            .and_then(parse_string)
            .unwrap_or_else(|| "rgba(96,165,250,0.5)".into()),
    })
}

fn parse_vector_3d(arg: &str, vars: &BTreeMap<String, f64>) -> Option<Vector3D> {
    Some(Vector3D {
        from: prop(arg, "from")
            .and_then(|v| parse_triple(v, vars))
            .unwrap_or((0.0, 0.0, 0.0)),
        to: prop(arg, "to").and_then(|v| parse_triple(v, vars))?,
        color: prop(arg, "color")
            .and_then(parse_string)
            .unwrap_or_else(|| COLORS[2].into()),
        label: prop(arg, "label").and_then(parse_string),
    })
}

fn parse_point_3d(arg: &str, vars: &BTreeMap<String, f64>) -> Option<Point3D> {
    Some(Point3D {
        at: prop(arg, "at").and_then(|v| parse_triple(v, vars))?,
        color: prop(arg, "color")
            .and_then(parse_string)
            .unwrap_or_else(|| COLORS[1].into()),
        radius: prop(arg, "radius")
            .and_then(|v| eval_expr(v, vars).ok())
            .unwrap_or(4.0)
            .clamp(1.0, 14.0),
        label: prop(arg, "label").and_then(parse_string),
    })
}

fn parse_particles_3d(arg: &str, vars: &BTreeMap<String, f64>) -> Option<Particles3D> {
    let positions = prop(arg, "positions")
        .and_then(|v| parse_triples(v, vars))
        .unwrap_or_default()
        .into_iter()
        .take(500)
        .collect::<Vec<_>>();
    (!positions.is_empty()).then_some(Particles3D {
        positions,
        color: prop(arg, "color")
            .and_then(parse_string)
            .unwrap_or_else(|| COLORS[3].into()),
        size: prop(arg, "size")
            .and_then(|v| eval_expr(v, vars).ok())
            .unwrap_or(2.0)
            .clamp(0.5, 8.0),
    })
}

fn bounded_samples(
    raw: Option<&str>,
    vars: &BTreeMap<String, f64>,
    min: usize,
    max: usize,
    budget: &mut usize,
) -> usize {
    let requested = raw
        .and_then(|v| eval_expr(v, vars).ok())
        .map(|v| v.round() as usize)
        .unwrap_or(SAMPLES)
        .clamp(min, max);
    let samples = requested.min(*budget).max(2);
    *budget = budget.saturating_sub(samples);
    samples
}

fn prop<'a>(object: &'a str, key: &str) -> Option<&'a str> {
    let object = object.trim().trim_start_matches('{').trim_end_matches('}');
    let bytes = object.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        let found = &object[start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b':' {
            i += 1;
            continue;
        }
        i += 1;
        let value_start = i;
        let mut depth = 0_i32;
        let mut quote: Option<u8> = None;
        let mut escape = false;
        while i < bytes.len() {
            let b = bytes[i];
            if let Some(q) = quote {
                if escape {
                    escape = false;
                } else if b == b'\\' {
                    escape = true;
                } else if b == q {
                    quote = None;
                }
                i += 1;
                continue;
            }
            match b {
                b'"' | b'\'' | b'`' => quote = Some(b),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b',' if depth <= 0 => break,
                _ => {}
            }
            i += 1;
        }
        if found == key {
            return Some(object[value_start..i].trim());
        }
        i += 1;
    }
    None
}

fn parse_expr_value(value: &str, arg_name: &str) -> Option<String> {
    if let Some(s) = parse_string(value) {
        return Some(normalize_expr(&s));
    }
    parse_arrow_expr(value, arg_name)
}

fn parse_arrow_expr(value: &str, _arg_name: &str) -> Option<String> {
    let (_, expr) = value.split_once("=>")?;
    Some(normalize_expr(expr.trim().trim_matches(['{', '}']).trim()))
}

fn parse_arrow_array(value: &str, _first_arg_name: &str) -> Option<(String, String)> {
    let (_, expr) = value.split_once("=>")?;
    parse_expr_array(expr)
}

fn parse_arrow_array3(value: &str) -> Option<(String, String, String)> {
    let (_, expr) = value.split_once("=>")?;
    parse_expr_array3(expr)
}

fn parse_expr_array(expr: &str) -> Option<(String, String)> {
    let inner = expr
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim();
    let (a, b) = split_top_level_once(inner, ',')?;
    Some((normalize_expr(a), normalize_expr(b)))
}

fn parse_expr_array3(expr: &str) -> Option<(String, String, String)> {
    let inner = expr
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim();
    let (a, rest) = split_top_level_once(inner, ',')?;
    let (b, c) = split_top_level_once(rest, ',')?;
    Some((normalize_expr(a), normalize_expr(b), normalize_expr(c)))
}

fn split_top_level_once(s: &str, sep: char) -> Option<(&str, &str)> {
    let mut depth = 0_i32;
    let mut quote: Option<char> = None;
    let mut escape = false;
    for (idx, ch) in s.char_indices() {
        if let Some(q) = quote {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' | '`' => quote = Some(ch),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            c if c == sep && depth == 0 => return Some((&s[..idx], &s[idx + 1..])),
            _ => {}
        }
    }
    None
}

fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut depth = 0_i32;
    let mut quote: Option<char> = None;
    let mut escape = false;
    for (idx, ch) in s.char_indices() {
        if let Some(q) = quote {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' | '`' => quote = Some(ch),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(s[start..idx].trim());
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

fn parse_pair(value: &str, vars: &BTreeMap<String, f64>) -> Option<(f64, f64)> {
    let inner = value
        .trim()
        .trim_start_matches('[')
        .trim_start_matches('(')
        .trim_end_matches(']')
        .trim_end_matches(')');
    let (a, b) = split_top_level_once(inner, ',')?;
    Some((eval_expr(a, vars).ok()?, eval_expr(b, vars).ok()?))
}

fn parse_triple(value: &str, vars: &BTreeMap<String, f64>) -> Option<(f64, f64, f64)> {
    let inner = value
        .trim()
        .trim_start_matches('[')
        .trim_start_matches('(')
        .trim_end_matches(']')
        .trim_end_matches(')');
    let (a, rest) = split_top_level_once(inner, ',')?;
    let (b, c) = split_top_level_once(rest, ',')?;
    Some((
        eval_expr(a, vars).ok()?,
        eval_expr(b, vars).ok()?,
        eval_expr(c, vars).ok()?,
    ))
}

fn parse_triples(value: &str, vars: &BTreeMap<String, f64>) -> Option<Vec<(f64, f64, f64)>> {
    let value = value.trim();
    let inner = value.strip_prefix('[')?.strip_suffix(']')?.trim();
    let points = split_top_level(inner, ',')
        .into_iter()
        .filter_map(|part| parse_triple(part, vars))
        .collect::<Vec<_>>();
    (!points.is_empty()).then_some(points)
}

fn parse_string(value: &str) -> Option<String> {
    let value = value.trim();
    let quote = value.chars().next()?;
    if !matches!(quote, '"' | '\'' | '`') || !value.ends_with(quote) {
        return None;
    }
    Some(value[1..value.len() - 1].to_string())
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn normalize_expr(expr: &str) -> String {
    expr.trim()
        .trim_end_matches(';')
        .replace("Math.", "")
        .replace("**", "^")
        .replace("=>", "")
}

fn eval_expr(expr: &str, vars: &BTreeMap<String, f64>) -> Result<f64, ()> {
    let expr = normalize_expr(expr);
    if !expr.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                ' ' | '\t'
                    | '\n'
                    | '\r'
                    | '.'
                    | ','
                    | '_'
                    | '+'
                    | '-'
                    | '*'
                    | '/'
                    | '^'
                    | '%'
                    | '('
                    | ')'
            )
    }) {
        return Err(());
    }
    let mut ns = vars.clone();
    fasteval::ez_eval(&expr, &mut ns).map_err(|_| ())
}

fn eval_calc_call(expr: &str, vars: &BTreeMap<String, f64>) -> Option<f64> {
    let (name, raw_args) = expr.trim().strip_prefix("calc.")?.split_once('(')?;
    let raw_args = raw_args.trim().strip_suffix(')')?;
    let (fn_expr, rest) = split_top_level_once(raw_args, ',')?;
    let fn_expr = parse_string(fn_expr).map(|expr| normalize_expr(&expr))?;
    match name.trim() {
        "derivative" => {
            let x = eval_expr(rest, vars).ok()?;
            numeric_derivative(&fn_expr, x)
        }
        "integral" => {
            let (a, b) = split_top_level_once(rest, ',')?;
            numeric_integral(&fn_expr, eval_expr(a, vars).ok()?, eval_expr(b, vars).ok()?)
        }
        "root" => {
            let (a, b) = split_top_level_once(rest, ',')?;
            numeric_root(&fn_expr, eval_expr(a, vars).ok()?, eval_expr(b, vars).ok()?)
        }
        _ => None,
    }
}

fn numeric_derivative(expr: &str, x: f64) -> Option<f64> {
    let h = (x.abs() + 1.0) * 1e-5;
    let mut vars = default_vars();
    vars.insert("x".into(), x + h);
    let a = eval_expr(expr, &vars).ok()?;
    vars.insert("x".into(), x - h);
    let b = eval_expr(expr, &vars).ok()?;
    Some((a - b) / (2.0 * h))
}

fn numeric_integral(expr: &str, a: f64, b: f64) -> Option<f64> {
    let n = 512;
    let h = (b - a) / n as f64;
    let mut vars = default_vars();
    let mut sum = 0.0;
    for i in 0..=n {
        let x = a + h * i as f64;
        vars.insert("x".into(), x);
        let weight = if i == 0 || i == n {
            1.0
        } else if i % 2 == 0 {
            2.0
        } else {
            4.0
        };
        sum += weight * eval_expr(expr, &vars).ok()?;
    }
    Some(sum * h / 3.0)
}

fn numeric_root(expr: &str, mut a: f64, mut b: f64) -> Option<f64> {
    let mut vars = default_vars();
    vars.insert("x".into(), a);
    let mut fa = eval_expr(expr, &vars).ok()?;
    vars.insert("x".into(), b);
    let fb = eval_expr(expr, &vars).ok()?;
    if fa.abs() < 1e-9 {
        return Some(a);
    }
    if fb.abs() < 1e-9 {
        return Some(b);
    }
    if fa.signum() == fb.signum() {
        return None;
    }
    for _ in 0..80 {
        let mid = (a + b) * 0.5;
        vars.insert("x".into(), mid);
        let fm = eval_expr(expr, &vars).ok()?;
        if fm.abs() < 1e-9 {
            return Some(mid);
        }
        if fa.signum() == fm.signum() {
            a = mid;
            fa = fm;
        } else {
            b = mid;
        }
    }
    Some((a + b) * 0.5)
}

fn interpolate_vars(text: &str, vars: &BTreeMap<String, f64>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let (before, after_start) = rest.split_at(start);
        out.push_str(before);
        if let Some(end) = after_start.find('}') {
            let name = after_start[1..end].trim();
            if let Some(value) = vars.get(name) {
                out.push_str(&format_number(*value));
            } else {
                out.push_str(&after_start[..=end]);
            }
            rest = &after_start[end + 1..];
        } else {
            out.push_str(after_start);
            return out;
        }
    }
    out.push_str(rest);
    out
}

fn format_number(value: f64) -> String {
    let rounded = format!("{value:.4}");
    rounded
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

fn scene_to_svg(scene: &MathStudioScene, view: MathStudioView) -> String {
    if scene.is_3d {
        return scene_3d_to_svg(scene, view);
    }
    let zoom = view.zoom.clamp(0.2, 20.0);
    let base_x = scene.x.1 - scene.x.0;
    let base_y = scene.y.1 - scene.y.0;
    let x_span = base_x / zoom;
    let y_span = base_y / zoom;
    let x_center = (scene.x.0 + scene.x.1) * 0.5 - view.pan_x * base_x / zoom;
    let y_center = (scene.y.0 + scene.y.1) * 0.5 + view.pan_y * base_y / zoom;
    let view_x = (x_center - x_span * 0.5, x_center + x_span * 0.5);
    let view_y = (y_center - y_span * 0.5, y_center + y_span * 0.5);
    let sx = |x: f64| MARGIN + (x - view_x.0) / (view_x.1 - view_x.0) * (W - 2.0 * MARGIN);
    let sy = |y: f64| (H - MARGIN) - (y - view_y.0) / (view_y.1 - view_y.0) * (H - 2.0 * MARGIN);
    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {W} {H}\">"
    ));
    svg.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{W}\" height=\"{H}\" rx=\"10\" fill=\"{}\"/>",
        esc(&scene.background)
    ));
    draw_grid(scene, view_x, view_y, &sx, &sy, &mut svg);
    for field in &scene.fields {
        draw_vector_field(scene, field, &sx, &sy, &mut svg);
    }
    for function in &scene.functions {
        draw_function(scene, function, view_x, &sx, &sy, &mut svg);
    }
    for param in &scene.params {
        draw_parametric(param, &sx, &sy, &mut svg);
    }
    for vector in &scene.vectors {
        draw_arrow(
            vector.from,
            vector.to,
            &vector.color,
            2.0,
            &sx,
            &sy,
            &mut svg,
        );
        if let Some(label) = &vector.label {
            draw_label(vector.to, label, &vector.color, &sx, &sy, &mut svg);
        }
    }
    for point in &scene.points {
        svg.push_str(&format!(
            "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"{:.1}\" fill=\"{}\"/>",
            sx(point.at.0),
            sy(point.at.1),
            point.radius,
            esc(&point.color)
        ));
        if let Some(label) = &point.label {
            draw_label(point.at, label, &point.color, &sx, &sy, &mut svg);
        }
    }
    for text in &scene.texts {
        svg.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{}\" font-size=\"{:.1}\" font-family=\"monospace\">{}</text>",
            sx(text.at.0),
            sy(text.at.1),
            esc(&text.color),
            text.size,
            esc(&text.text)
        ));
    }
    draw_diagnostics(&scene.diagnostics, &mut svg);
    svg.push_str("</svg>");
    svg
}

type Point3 = (f64, f64, f64);
type Arrow3 = (Point3, Point3);

struct Surface3DGeometry {
    points: Vec<Vec<Option<Point3>>>,
}

struct Scene3DGeometry {
    surfaces: Vec<Surface3DGeometry>,
    curves: Vec<Vec<Point3>>,
    fields: Vec<Vec<Arrow3>>,
}

impl Scene3DGeometry {
    fn new(scene: &MathStudioScene) -> Self {
        Self {
            surfaces: scene
                .surfaces_3d
                .iter()
                .map(|surface| {
                    let n = surface.segments.max(2);
                    let points = (0..=n)
                        .map(|ix| {
                            (0..=n)
                                .map(|iy| surface_point(surface, ix, iy, n))
                                .collect()
                        })
                        .collect();
                    Surface3DGeometry { points }
                })
                .collect(),
            curves: scene.curves_3d.iter().map(curve_points).collect(),
            fields: scene.fields_3d.iter().map(vector_field_3d_arrows).collect(),
        }
    }
}

struct Projector {
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
    view: MathStudioView,
}

impl Projector {
    fn new(scene: &MathStudioScene, geometry: &Scene3DGeometry, view: MathStudioView) -> Self {
        let mut projector = Self {
            min_x: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            min_y: f64::INFINITY,
            max_y: f64::NEG_INFINITY,
            view,
        };
        if scene.axes_3d {
            let s = scene.size_3d;
            for point in [(0.0, 0.0, 0.0), (s, 0.0, 0.0), (0.0, s, 0.0), (0.0, 0.0, s)] {
                projector.include(point);
            }
        }
        for surface in &geometry.surfaces {
            for row in &surface.points {
                for point in row.iter().flatten() {
                    projector.include(*point);
                }
            }
        }
        for curve in &geometry.curves {
            for point in curve {
                projector.include(*point);
            }
        }
        for field in &geometry.fields {
            for (from, to) in field {
                projector.include(*from);
                projector.include(*to);
            }
        }
        for vector in &scene.vectors_3d {
            projector.include(vector.from);
            projector.include(vector.to);
        }
        for point in &scene.points_3d {
            projector.include(point.at);
        }
        for particles in &scene.particles_3d {
            for point in &particles.positions {
                projector.include(*point);
            }
        }
        for text in &scene.texts {
            projector.include((text.at.0, text.at.1, 0.0));
        }
        if !projector.min_x.is_finite() {
            let s = scene.size_3d;
            for point in [(-s, -s, -s), (s, s, s)] {
                projector.include(point);
            }
        }
        projector.pad()
    }

    fn include(&mut self, point: (f64, f64, f64)) {
        let (x, y) = project_3d(point, self.view);
        if x.is_finite() && y.is_finite() {
            self.min_x = self.min_x.min(x);
            self.max_x = self.max_x.max(x);
            self.min_y = self.min_y.min(y);
            self.max_y = self.max_y.max(y);
        }
    }

    fn pad(mut self) -> Self {
        if (self.max_x - self.min_x).abs() < 1e-6 {
            self.min_x -= 1.0;
            self.max_x += 1.0;
        }
        if (self.max_y - self.min_y).abs() < 1e-6 {
            self.min_y -= 1.0;
            self.max_y += 1.0;
        }
        let pad_x = (self.max_x - self.min_x) * 0.08;
        let pad_y = (self.max_y - self.min_y) * 0.08;
        self.min_x -= pad_x;
        self.max_x += pad_x;
        self.min_y -= pad_y;
        self.max_y += pad_y;
        self
    }

    fn screen(&self, point: (f64, f64, f64)) -> (f64, f64) {
        let (x, y) = project_3d(point, self.view);
        let px = MARGIN + (x - self.min_x) / (self.max_x - self.min_x) * (W - 2.0 * MARGIN);
        let py = (H - MARGIN) - (y - self.min_y) / (self.max_y - self.min_y) * (H - 2.0 * MARGIN);
        (
            W * 0.5 + (px - W * 0.5) * self.view.zoom.clamp(0.2, 20.0) + self.view.pan_x * W,
            H * 0.5 + (py - H * 0.5) * self.view.zoom.clamp(0.2, 20.0) + self.view.pan_y * H,
        )
    }
}

fn scene_3d_to_svg(scene: &MathStudioScene, view: MathStudioView) -> String {
    let geometry = Scene3DGeometry::new(scene);
    let projector = Projector::new(scene, &geometry, view);
    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {W} {H}\">"
    ));
    svg.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{W}\" height=\"{H}\" rx=\"10\" fill=\"{}\"/>",
        esc(&scene.background)
    ));
    for (surface, surface_geometry) in scene.surfaces_3d.iter().zip(&geometry.surfaces) {
        draw_surface_3d(surface, surface_geometry, &projector, &mut svg);
    }
    for (field, arrows) in scene.fields_3d.iter().zip(&geometry.fields) {
        draw_vector_field_3d(field, arrows, &projector, &mut svg);
    }
    if scene.axes_3d {
        draw_axes_3d(scene.size_3d, &projector, &mut svg);
    }
    for (curve, points) in scene.curves_3d.iter().zip(&geometry.curves) {
        draw_curve_3d(curve, points, &projector, &mut svg);
    }
    for vector in &scene.vectors_3d {
        draw_arrow_screen(
            projector.screen(vector.from),
            projector.screen(vector.to),
            &vector.color,
            2.0,
            &mut svg,
        );
        if let Some(label) = &vector.label {
            draw_text_screen(
                projector.screen(vector.to),
                label,
                &vector.color,
                12.0,
                &mut svg,
            );
        }
    }
    for point in &scene.points_3d {
        let (x, y) = projector.screen(point.at);
        svg.push_str(&format!(
            "<circle cx=\"{x:.1}\" cy=\"{y:.1}\" r=\"{:.1}\" fill=\"{}\"/>",
            point.radius,
            esc(&point.color)
        ));
        if let Some(label) = &point.label {
            draw_text_screen((x, y), label, &point.color, 12.0, &mut svg);
        }
    }
    for particles in &scene.particles_3d {
        for point in &particles.positions {
            let (x, y) = projector.screen(*point);
            svg.push_str(&format!(
                "<circle cx=\"{x:.1}\" cy=\"{y:.1}\" r=\"{:.1}\" fill=\"{}\" opacity=\"0.86\"/>",
                particles.size,
                esc(&particles.color)
            ));
        }
    }
    for text in &scene.texts {
        draw_text_screen(
            projector.screen((text.at.0, text.at.1, 0.0)),
            &text.text,
            &text.color,
            text.size,
            &mut svg,
        );
    }
    draw_diagnostics(&scene.diagnostics, &mut svg);
    svg.push_str("</svg>");
    svg
}

fn project_3d((x, y, z): (f64, f64, f64), view: MathStudioView) -> (f64, f64) {
    let yaw = view.orbit_x + std::f64::consts::FRAC_PI_4;
    let pitch = (view.orbit_y + 0.55).clamp(-1.25, 1.25);
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let x1 = x * cy - y * sy;
    let y1 = x * sy + y * cy;
    let z1 = z;
    let y2 = y1 * cp - z1 * sp;
    let z2 = y1 * sp + z1 * cp;
    (x1, y2 - z2 * 0.55)
}

fn draw_axes_3d(size: f64, projector: &Projector, svg: &mut String) {
    for (to, color, label) in [
        ((size, 0.0, 0.0), "#ef4444", "x"),
        ((0.0, size, 0.0), "#22c55e", "y"),
        ((0.0, 0.0, size), "#60a5fa", "z"),
    ] {
        draw_arrow_screen(
            projector.screen((0.0, 0.0, 0.0)),
            projector.screen(to),
            color,
            1.6,
            svg,
        );
        draw_text_screen(projector.screen(to), label, color, 12.0, svg);
    }
}

fn draw_surface_3d(
    surface: &Surface3D,
    geometry: &Surface3DGeometry,
    projector: &Projector,
    svg: &mut String,
) {
    for row in &geometry.points {
        draw_polyline_3d(
            row.iter().filter_map(|point| *point),
            projector,
            &surface.color,
            1.0,
            surface.opacity,
            svg,
        );
    }
    let columns = geometry
        .points
        .iter()
        .map(|row| row.len())
        .max()
        .unwrap_or(0);
    for iy in 0..columns {
        draw_polyline_3d(
            geometry
                .points
                .iter()
                .filter_map(|row| row.get(iy).copied().flatten()),
            projector,
            &surface.color,
            1.0,
            surface.opacity,
            svg,
        );
    }
}

fn draw_curve_3d(curve: &Curve3D, points: &[Point3], projector: &Projector, svg: &mut String) {
    draw_polyline_3d(
        points.iter().copied(),
        projector,
        &curve.color,
        curve.width,
        1.0,
        svg,
    );
}

fn draw_vector_field_3d(
    field: &VectorField3D,
    arrows: &[Arrow3],
    projector: &Projector,
    svg: &mut String,
) {
    for (from, to) in arrows {
        draw_arrow_screen(
            projector.screen(*from),
            projector.screen(*to),
            &field.color,
            1.0,
            svg,
        );
    }
}

fn draw_polyline_3d(
    points: impl Iterator<Item = (f64, f64, f64)>,
    projector: &Projector,
    color: &str,
    width: f64,
    opacity: f64,
    svg: &mut String,
) {
    let mut point_count = 0;
    let mut point_data = String::new();
    for point in points {
        let (x, y) = projector.screen(point);
        let _ = write!(&mut point_data, "{x:.1},{y:.1} ");
        point_count += 1;
    }
    if point_count > 1 {
        svg.push_str(&format!(
            "<polyline fill=\"none\" stroke=\"{}\" stroke-width=\"{width:.1}\" opacity=\"{opacity:.2}\" points=\"{}\"/>",
            esc(color),
            point_data.trim_end()
        ));
    }
}

fn surface_point(surface: &Surface3D, ix: usize, iy: usize, n: usize) -> Option<(f64, f64, f64)> {
    let x = surface.x.0 + (surface.x.1 - surface.x.0) * ix as f64 / n as f64;
    let y = surface.y.0 + (surface.y.1 - surface.y.0) * iy as f64 / n as f64;
    let mut vars = surface.vars.clone();
    vars.insert("x".into(), x);
    vars.insert("y".into(), y);
    let z = eval_expr(&surface.z_expr, &vars).ok()?;
    z.is_finite().then_some((x, y, z))
}

fn curve_points(curve: &Curve3D) -> Vec<(f64, f64, f64)> {
    let mut out = Vec::new();
    let mut vars = curve.vars.clone();
    let samples = curve.samples.max(2);
    for i in 0..samples {
        let t = curve.t.0 + (curve.t.1 - curve.t.0) * i as f64 / (samples - 1) as f64;
        vars.insert("t".into(), t);
        if let (Ok(x), Ok(y), Ok(z)) = (
            eval_expr(&curve.x_expr, &vars),
            eval_expr(&curve.y_expr, &vars),
            eval_expr(&curve.z_expr, &vars),
        ) && x.is_finite()
            && y.is_finite()
            && z.is_finite()
        {
            out.push((x, y, z));
        }
    }
    out
}

fn vector_field_3d_arrows(field: &VectorField3D) -> Vec<((f64, f64, f64), (f64, f64, f64))> {
    let mut out = Vec::new();
    let n = field.density.max(2);
    let mut vars = field.vars.clone();
    for ix in 0..n {
        for iy in 0..n {
            for iz in 0..n {
                let x = field.bounds.0
                    + (field.bounds.1 - field.bounds.0) * (ix as f64 + 0.5) / n as f64;
                let y = field.bounds.0
                    + (field.bounds.1 - field.bounds.0) * (iy as f64 + 0.5) / n as f64;
                let z = field.bounds.0
                    + (field.bounds.1 - field.bounds.0) * (iz as f64 + 0.5) / n as f64;
                vars.insert("x".into(), x);
                vars.insert("y".into(), y);
                vars.insert("z".into(), z);
                if let (Ok(vx), Ok(vy), Ok(vz)) = (
                    eval_expr(&field.x_expr, &vars),
                    eval_expr(&field.y_expr, &vars),
                    eval_expr(&field.z_expr, &vars),
                ) && vx.is_finite()
                    && vy.is_finite()
                    && vz.is_finite()
                {
                    out.push((
                        (x, y, z),
                        (
                            x + vx * field.scale,
                            y + vy * field.scale,
                            z + vz * field.scale,
                        ),
                    ));
                }
            }
        }
    }
    out
}

fn draw_grid(
    scene: &MathStudioScene,
    x_range: (f64, f64),
    y_range: (f64, f64),
    sx: &impl Fn(f64) -> f64,
    sy: &impl Fn(f64) -> f64,
    svg: &mut String,
) {
    if scene.grid {
        let step = scene.step.max(0.0001);
        let mut x = (x_range.0 / step).ceil() * step;
        while x <= x_range.1 {
            svg.push_str(&format!(
                "<line x1=\"{:.1}\" y1=\"{MARGIN}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"#253044\" stroke-width=\"0.8\"/>",
                sx(x),
                sx(x),
                H - MARGIN
            ));
            x += step;
        }
        let mut y = (y_range.0 / step).ceil() * step;
        while y <= y_range.1 {
            svg.push_str(&format!(
                "<line x1=\"{MARGIN}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"#253044\" stroke-width=\"0.8\"/>",
                sy(y),
                W - MARGIN,
                sy(y)
            ));
            y += step;
        }
    }
    if scene.axes {
        if x_range.0 < 0.0 && x_range.1 > 0.0 {
            svg.push_str(&format!(
                "<line x1=\"{:.1}\" y1=\"{MARGIN}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"#5b6b85\" stroke-width=\"1.2\"/>",
                sx(0.0),
                sx(0.0),
                H - MARGIN
            ));
        }
        if y_range.0 < 0.0 && y_range.1 > 0.0 {
            svg.push_str(&format!(
                "<line x1=\"{MARGIN}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"#5b6b85\" stroke-width=\"1.2\"/>",
                sy(0.0),
                W - MARGIN,
                sy(0.0)
            ));
        }
    }
}

fn draw_function(
    _scene: &MathStudioScene,
    function: &Function2D,
    view_x: (f64, f64),
    sx: &impl Fn(f64) -> f64,
    sy: &impl Fn(f64) -> f64,
    svg: &mut String,
) {
    let (xmin, xmax) = function.domain.unwrap_or(view_x);
    let samples = function.samples.max(2);
    let mut pts = Vec::new();
    let mut vars = function.vars.clone();
    for i in 0..samples {
        let x = xmin + (xmax - xmin) * (i as f64) / ((samples - 1) as f64);
        vars.insert("x".into(), x);
        let Ok(y) = eval_expr(&function.expr, &vars) else {
            continue;
        };
        if y.is_finite() {
            pts.push(format!("{:.1},{:.1}", sx(x), sy(y)));
        }
    }
    if !pts.is_empty() {
        svg.push_str(&format!(
            "<polyline fill=\"none\" stroke=\"{}\" stroke-width=\"{:.1}\" points=\"{}\"/>",
            esc(&function.color),
            function.width,
            pts.join(" ")
        ));
        if let Some(label) = &function.label {
            draw_label(
                (
                    xmin,
                    eval_expr_at_x(&function.expr, xmin, &function.vars).unwrap_or(0.0),
                ),
                label,
                &function.color,
                sx,
                sy,
                svg,
            );
        }
    }
}

fn draw_parametric(
    param: &Parametric2D,
    sx: &impl Fn(f64) -> f64,
    sy: &impl Fn(f64) -> f64,
    svg: &mut String,
) {
    let mut pts = Vec::new();
    let mut vars = param.vars.clone();
    let samples = param.samples.max(2);
    for i in 0..samples {
        let t = param.t.0 + (param.t.1 - param.t.0) * (i as f64) / ((samples - 1) as f64);
        vars.insert("t".into(), t);
        if let (Ok(x), Ok(y)) = (
            eval_expr(&param.x_expr, &vars),
            eval_expr(&param.y_expr, &vars),
        ) && x.is_finite()
            && y.is_finite()
        {
            pts.push(format!("{:.1},{:.1}", sx(x), sy(y)));
        }
    }
    if !pts.is_empty() {
        svg.push_str(&format!(
            "<polyline fill=\"none\" stroke=\"{}\" stroke-width=\"{:.1}\" points=\"{}\"/>",
            esc(&param.color),
            param.width,
            pts.join(" ")
        ));
    }
}

fn draw_vector_field(
    scene: &MathStudioScene,
    field: &VectorField2D,
    sx: &impl Fn(f64) -> f64,
    sy: &impl Fn(f64) -> f64,
    svg: &mut String,
) {
    let n = field.density.max(2);
    let mut vars = field.vars.clone();
    for ix in 0..n {
        for iy in 0..n {
            let x = scene.x.0 + (scene.x.1 - scene.x.0) * (ix as f64 + 0.5) / (n as f64);
            let y = scene.y.0 + (scene.y.1 - scene.y.0) * (iy as f64 + 0.5) / (n as f64);
            vars.insert("x".into(), x);
            vars.insert("y".into(), y);
            if let (Ok(vx), Ok(vy)) = (
                eval_expr(&field.x_expr, &vars),
                eval_expr(&field.y_expr, &vars),
            ) && vx.is_finite()
                && vy.is_finite()
            {
                draw_arrow(
                    (x, y),
                    (x + vx * field.scale, y + vy * field.scale),
                    &field.color,
                    1.0,
                    sx,
                    sy,
                    svg,
                );
            }
        }
    }
}

fn draw_arrow(
    from: (f64, f64),
    to: (f64, f64),
    color: &str,
    width: f64,
    sx: &impl Fn(f64) -> f64,
    sy: &impl Fn(f64) -> f64,
    svg: &mut String,
) {
    draw_arrow_screen(
        (sx(from.0), sy(from.1)),
        (sx(to.0), sy(to.1)),
        color,
        width,
        svg,
    );
}

fn draw_arrow_screen(from: (f64, f64), to: (f64, f64), color: &str, width: f64, svg: &mut String) {
    let (x1, y1, x2, y2) = (from.0, from.1, to.0, to.1);
    svg.push_str(&format!(
        "<line x1=\"{x1:.1}\" y1=\"{y1:.1}\" x2=\"{x2:.1}\" y2=\"{y2:.1}\" stroke=\"{}\" stroke-width=\"{width:.1}\" stroke-linecap=\"round\"/>",
        esc(color)
    ));
    let angle = (y2 - y1).atan2(x2 - x1);
    let head = 7.0;
    for delta in [2.65, -2.65] {
        let a = angle + delta;
        svg.push_str(&format!(
            "<line x1=\"{x2:.1}\" y1=\"{y2:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"{}\" stroke-width=\"{width:.1}\" stroke-linecap=\"round\"/>",
            x2 + head * a.cos(),
            y2 + head * a.sin(),
            esc(color)
        ));
    }
}

fn draw_text_screen(at: (f64, f64), text: &str, color: &str, size: f64, svg: &mut String) {
    svg.push_str(&format!(
        "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{}\" font-size=\"{size:.1}\" font-family=\"monospace\">{}</text>",
        at.0 + 6.0,
        at.1 - 6.0,
        esc(color),
        esc(text)
    ));
}

fn draw_label(
    at: (f64, f64),
    label: &str,
    color: &str,
    sx: &impl Fn(f64) -> f64,
    sy: &impl Fn(f64) -> f64,
    svg: &mut String,
) {
    svg.push_str(&format!(
        "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{}\" font-size=\"12\" font-family=\"monospace\">{}</text>",
        sx(at.0) + 6.0,
        sy(at.1) - 6.0,
        esc(color),
        esc(label)
    ));
}

fn draw_diagnostics(diagnostics: &[String], svg: &mut String) {
    if diagnostics.is_empty() {
        return;
    }
    let text = diagnostics
        .iter()
        .take(3)
        .map(|d| compact(d))
        .collect::<Vec<_>>()
        .join(" | ");
    svg.push_str(&format!(
        "<rect x=\"16\" y=\"{:.1}\" width=\"608\" height=\"28\" rx=\"6\" fill=\"#1f2937\" opacity=\"0.92\"/>",
        H - 44.0
    ));
    svg.push_str(&format!(
        "<text x=\"28\" y=\"{:.1}\" fill=\"#fbbf24\" font-size=\"12\" font-family=\"monospace\">{}</text>",
        H - 26.0,
        esc(&text)
    ));
}

fn eval_expr_at_x(expr: &str, x: f64, base_vars: &BTreeMap<String, f64>) -> Option<f64> {
    let mut vars = base_vars.clone();
    vars.insert("x".into(), x);
    eval_expr(expr, &vars).ok()
}

fn compact(s: &str) -> String {
    let mut out = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.len() > 80 {
        out.truncate(77);
        out.push_str("...");
    }
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
        assert!(is_rich_lang("mathstudio"));
        assert!(!is_rich_lang("rust"));
        assert!(render_rich_svg("rust", "fn main() {}").is_none());
    }

    #[test]
    fn mathstudio_renders_code_mentor_style_2d_scene() {
        let svg = render_rich_svg(
            "mathstudio",
            r##"
scene.background("#0d1117");
scene.cartesianPlane({ x: [-6, 6], y: [-3, 3], step: 1 });
scene.function({ expression: x => Math.sin(x), color: "#3b82f6", label: "sin(x)" });
scene.parametric({ fn: t => [Math.cos(t), Math.sin(t)], t: [0, Math.PI*2], color: "#a855f7" });
scene.vector({ from: [0,0], to: [2,1], color: "#22c55e", label: "v" });
scene.point({ at: [Math.PI, 0], color: "#ef4444", label: "pi" });
scene.text({ at: [-5, 2.5], text: "Math Studio", color: "#cbd5e1" });
"##,
        )
        .expect("mathstudio renderiza");
        assert!(svg.contains("<polyline"));
        assert!(svg.contains("<circle"));
        assert!(svg.contains("Math Studio"));
    }

    #[test]
    fn mathstudio_blocks_unsafe_javascript() {
        let svg = render_rich_svg("mathstudio", r#"fetch("https://example.com");"#)
            .expect("diagnostico renderiza");
        assert!(svg.contains("comando bloqueado"));
    }

    #[test]
    fn mathstudio_renders_static_3d_scene() {
        let svg = render_rich_svg(
            "mathstudio",
            r##"
scene3d.background("#0d1117");
scene3d.axes({ size: 5 });
let phase = 0;
scene3d.animate(t => { phase = t * 0.5; });
scene3d.surface({
  fn: (x, y) => Math.sin(x + phase) * Math.cos(y),
  x: [-4, 4], y: [-4, 4],
  segments: 12,
  color: "#3b82f6",
  opacity: 0.9
});
scene3d.curve3D({
  fn: t => [Math.cos(t)*2, t*0.2 - 2, Math.sin(t)*2],
  t: [0, Math.PI * 4],
  samples: 80,
  color: "#a855f7"
});
scene3d.vectorField3D({
  fn: (x, y, z) => [-y * 0.2, 0, x * 0.2],
  bounds: [-2, 2],
  density: 3,
  scale: 0.6,
  color: "#60a5fa"
});
scene3d.point3D({ at: [0, 0, 0], color: "#ef4444", radius: 2 });
"##,
        )
        .expect("mathstudio 3d renderiza");
        assert!(svg.contains("<polyline"));
        assert!(svg.contains("<circle"));
        assert!(!svg.contains("preview 3D"));
    }

    #[test]
    fn mathstudio_renders_calc_helpers_in_text() {
        let svg = render_rich_svg(
            "mathstudio",
            r##"
const area = calc.integral("sin(x)", 0, Math.PI);
const slope = calc.derivative("x^2", 3);
const zero = calc.root("x^2 - 2", 1, 2);
scene.cartesianPlane({ x: [-1, 8], y: [-1, 4], step: 1 });
scene.text({ at: [0, 3], text: "area={area} slope={slope} root={zero}" });
"##,
        )
        .expect("mathstudio calculo renderiza");
        assert!(svg.contains("area=2"));
        assert!(svg.contains("slope=6"));
        assert!(svg.contains("root=1.4142"));
    }

    #[test]
    fn mathstudio_renders_inline_3d_particles() {
        let svg = render_rich_svg(
            "mathstudio",
            r##"
scene3d.axes({ size: 2 });
scene3d.particles({
  positions: [[0,0,0], [1,1,1], [-1,1,0]],
  color: "#22c55e",
  size: 2
});
"##,
        )
        .expect("mathstudio particles renderiza");
        assert_eq!(svg.matches("<circle").count(), 3);
    }

    #[test]
    fn mathstudio_animation_time_updates_variables() {
        let source = r##"
let phase = 0;
scene.animate(t => { phase = t; });
scene.cartesianPlane({ x: [-1, 3], y: [-1, 1] });
scene.point({ at: [phase, 0], color: "#ef4444", radius: 4 });
"##;
        let a = render_rich_svg_with_view("mathstudio", source, MathStudioView::default())
            .expect("frame inicial renderiza");
        let b = render_rich_svg_with_view(
            "mathstudio",
            source,
            MathStudioView {
                time: 2.0,
                ..Default::default()
            },
        )
        .expect("frame animado renderiza");
        assert_ne!(a, b);
        assert!(b.contains("<circle"));
    }

    #[test]
    fn mathstudio_view_changes_2d_and_3d_camera() {
        let source_2d = r##"
scene.cartesianPlane({ x: [-6, 6], y: [-3, 3] });
scene.function({ expression: x => Math.sin(x), color: "#3b82f6" });
"##;
        let normal = render_rich_svg_with_view("mathstudio", source_2d, MathStudioView::default())
            .expect("2d renderiza");
        let zoomed = render_rich_svg_with_view(
            "mathstudio",
            source_2d,
            MathStudioView {
                zoom: 2.0,
                pan_x: 0.2,
                ..Default::default()
            },
        )
        .expect("2d com zoom renderiza");
        assert_ne!(normal, zoomed);

        let source_3d = r##"
scene3d.axes({ size: 3 });
scene3d.curve3D({ fn: t => [Math.cos(t), t, Math.sin(t)], t: [0, Math.PI*2] });
"##;
        let a = render_rich_svg_with_view("mathstudio", source_3d, MathStudioView::default())
            .expect("3d renderiza");
        let b = render_rich_svg_with_view(
            "mathstudio",
            source_3d,
            MathStudioView {
                orbit_x: 0.8,
                orbit_y: 0.4,
                ..Default::default()
            },
        )
        .expect("3d orbit renderiza");
        assert_ne!(a, b);
    }

    #[test]
    fn mathstudio_function_uses_declared_vars() {
        let source = r##"
let phase = 1;
scene.cartesianPlane({ x: [0, 2], y: [0, 4] });
scene.function({ expression: x => x + phase, color: "#3b82f6", label: "f" });
"##;
        let hover = mathstudio_hover(source, MathStudioView::default(), W * 0.5, H * 0.5, W, H)
            .expect("hover renderiza");
        assert!((hover.samples[0].y - 2.0).abs() < 0.05);
    }

    #[test]
    fn mathstudio_hover_samples_2d_functions() {
        let hover = mathstudio_hover(
            r##"
scene.cartesianPlane({ x: [-1, 1], y: [-1, 1] });
scene.function({ expression: x => x * x, label: "x^2" });
"##,
            MathStudioView::default(),
            W * 0.5,
            H * 0.5,
            W,
            H,
        )
        .expect("hover 2d existe");
        assert_eq!(hover.samples[0].label.as_deref(), Some("x^2"));
        assert!(hover.samples[0].y.abs() < 0.01);
    }
}
